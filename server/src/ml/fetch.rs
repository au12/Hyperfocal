//! Downloads the AI models and runtimes into /config/ml: in pieces (so a stalled
//! connection costs one piece, and an interrupted download resumes where it stopped),
//! checked against their sha256 before they are moved into place, and wheels (zip files)
//! unpacked of only the libraries the engine needs.

use super::manifest::{Fetch, Wheel, file_name};
use anyhow::{Context, Result, anyhow, bail};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Bytes a request asks for at a time.
const PIECE: u64 = 32 << 20;

/// How far the downloads have got (bytes), and what is downloading.
#[derive(Default)]
pub struct Progress {
    pub done: AtomicU64,
    pub total: AtomicU64,
    pub current: parking_lot::Mutex<Option<String>>,
}

/// TLS roots: the built-in Mozilla set, or the bundle in SSL_CERT_FILE when one is set
/// (a proxy that inspects TLS, for example).
fn tls() -> ureq::tls::TlsConfig {
    let mut b = ureq::tls::TlsConfig::builder();
    if let Some(pem) = std::env::var_os("SSL_CERT_FILE").and_then(|p| fs::read(p).ok()) {
        let certs: Vec<_> = ureq::tls::parse_pem(&pem)
            .filter_map(|i| match i {
                Ok(ureq::tls::PemItem::Certificate(c)) => Some(c),
                _ => None,
            })
            .collect();
        if !certs.is_empty() {
            b = b.root_certs(ureq::tls::RootCerts::new_with_certs(&certs));
        }
    }
    b.build()
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .tls_config(tls())
        .timeout_connect(Some(Duration::from_secs(30)))
        // One piece: generous for a slow line, short enough that a stalled connection
        // is given up on and the piece asked for again.
        .timeout_global(Some(Duration::from_secs(600)))
        .http_status_as_error(false)
        .user_agent(concat!("hyperfocal/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

/// Downloads everything in `list`, reporting into `progress`.
pub fn fetch_all(list: &[Fetch], progress: &Progress) -> Result<()> {
    let agent = agent();
    progress.total.store(list.iter().map(|f| f.size()).sum(), Ordering::Relaxed);
    progress.done.store(0, Ordering::Relaxed);
    for f in list {
        match f {
            Fetch::Model { url, sha256, size, dest, what } => {
                *progress.current.lock() = Some(what.clone());
                download(&agent, url, sha256, *size, dest, progress).with_context(|| format!("downloading {}", file_name(url)))?;
            }
            Fetch::Wheel { wheel, dir } => {
                *progress.current.lock() = Some(format!("ONNX Runtime and GPU libraries ({})", wheel.name()));
                fetch_wheel(&agent, wheel, dir, progress).with_context(|| format!("downloading {}", wheel.name()))?;
            }
        }
    }
    *progress.current.lock() = None;
    Ok(())
}

/// Downloads `url` to `dest` (resuming a `.part` left by an earlier try), checks its
/// size and sha256, and only then moves it into place.
pub fn download(agent: &ureq::Agent, url: &str, sha256: &str, size: u64, dest: &Path, progress: &Progress) -> Result<()> {
    if let Some(dir) = dest.parent() {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let part = part_path(dest);
    let mut f = OpenOptions::new().create(true).read(true).append(true).open(&part).with_context(|| format!("opening {}", part.display()))?;
    let mut have = f.metadata()?.len();
    if have > size {
        f.set_len(0)?;
        have = 0;
    }
    // The bytes already there count towards the hash.
    let mut hasher = Sha256::new();
    if have > 0 {
        let mut r = File::open(&part)?;
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = r.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
    }
    progress.done.fetch_add(have, Ordering::Relaxed);
    let mut failures = 0;
    while have < size {
        let end = (have + PIECE).min(size) - 1;
        match piece(agent, url, have, end, size, &mut f, &mut hasher, progress) {
            Ok(n) => {
                have += n;
                failures = 0;
            }
            Err(e) => {
                failures += 1;
                if failures >= 5 {
                    return Err(e);
                }
                tracing::warn!("ml: {} (byte {have}): {e:#}; trying again", file_name(url));
                std::thread::sleep(Duration::from_secs(2u64.pow(failures)));
            }
        }
    }
    f.flush()?;
    f.sync_all()?;
    drop(f);
    let got = format!("{:x}", hasher.finalize());
    if got != sha256 {
        let _ = fs::remove_file(&part);
        bail!("the download does not match its sha256 (got {got}, expected {sha256}); it was deleted and will be fetched again");
    }
    fs::rename(&part, dest).with_context(|| format!("moving {} into place", dest.display()))?;
    Ok(())
}

/// One piece: bytes `from..=to`. Returns how many were written.
#[allow(clippy::too_many_arguments)]
fn piece(agent: &ureq::Agent, url: &str, from: u64, to: u64, size: u64, f: &mut File, hasher: &mut Sha256, progress: &Progress) -> Result<u64> {
    let mut r = agent.get(url).header("Range", &format!("bytes={from}-{to}")).call().map_err(|e| anyhow!("{e}"))?;
    let status = r.status().as_u16();
    let mut body = r.body_mut().with_config().limit(size + 1).reader();
    let mut buf = vec![0u8; 1 << 20];
    let mut n = 0u64;
    match status {
        206 => {}
        200 => {
            // The server ignored the range: skip what we have.
            let mut skip = from;
            while skip > 0 {
                let k = body.read(&mut buf[..skip.min(1 << 20) as usize])?;
                if k == 0 {
                    bail!("the download ended early");
                }
                skip -= k as u64;
            }
        }
        _ => bail!("the server answered {status}"),
    }
    let want = if status == 206 { to - from + 1 } else { size - from };
    while n < want {
        let k = body.read(&mut buf[..(want - n).min(1 << 20) as usize])?;
        if k == 0 {
            break;
        }
        f.write_all(&buf[..k])?;
        hasher.update(&buf[..k]);
        n += k as u64;
        progress.done.fetch_add(k as u64, Ordering::Relaxed);
    }
    if n == 0 {
        bail!("no data");
    }
    Ok(n)
}

fn part_path(dest: &Path) -> PathBuf {
    let mut s = dest.as_os_str().to_owned();
    s.push(".part");
    PathBuf::from(s)
}

/// Downloads a wheel and takes the listed libraries out of it into `dir`.
fn fetch_wheel(agent: &ureq::Agent, w: &Wheel, dir: &Path, progress: &Progress) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let tmp = dir.join(format!(".{}.whl", &w.sha256[..16]));
    download(agent, &w.url, &w.sha256, w.size, &tmp, progress)?;
    let res = extract(&tmp, &w.files, dir);
    let _ = fs::remove_file(&tmp);
    res?;
    // The wheel checked out and every file is in place.
    fs::write(dir.join(format!(".{}", &w.sha256[..16])), w.name())?;
    Ok(())
}

struct Entry {
    method: u16,
    crc: u32,
    csize: u64,
    usize: u64,
    offset: u64,
}

/// Takes `members` out of a zip file into `dir` (without their paths), checking each one's
/// CRC. Stored and deflated members, as wheels have.
pub fn extract(zip: &Path, members: &[String], dir: &Path) -> Result<()> {
    let mut f = File::open(zip)?;
    let len = f.metadata()?.len();
    // The end of central directory record: within the last 64 KB (plus its 22 bytes).
    let tail = len.min(65_557);
    f.seek(SeekFrom::Start(len - tail))?;
    let mut buf = vec![0u8; tail as usize];
    f.read_exact(&mut buf)?;
    let eocd = (0..buf.len().saturating_sub(21)).rev().find(|&i| buf[i..i + 4] == [0x50, 0x4b, 0x05, 0x06]).ok_or_else(|| anyhow!("not a zip file"))?;
    let u16at = |b: &[u8], i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
    let u32at = |b: &[u8], i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
    let count = u16at(&buf, eocd + 10) as usize;
    let cd_size = u32at(&buf, eocd + 12) as u64;
    let cd_off = u32at(&buf, eocd + 16) as u64;
    if cd_off == 0xFFFF_FFFF || count == 0xFFFF {
        bail!("ZIP64 archives are not supported");
    }
    f.seek(SeekFrom::Start(cd_off))?;
    let mut cd = vec![0u8; cd_size as usize];
    f.read_exact(&mut cd)?;
    let mut entries = std::collections::HashMap::new();
    let mut i = 0usize;
    while i + 46 <= cd.len() && cd[i..i + 4] == [0x50, 0x4b, 0x01, 0x02] {
        let method = u16at(&cd, i + 10);
        let crc = u32at(&cd, i + 16);
        let csize = u32at(&cd, i + 20) as u64;
        let usize = u32at(&cd, i + 24) as u64;
        let (n, e, c) = (u16at(&cd, i + 28) as usize, u16at(&cd, i + 30) as usize, u16at(&cd, i + 32) as usize);
        let offset = u32at(&cd, i + 42) as u64;
        let name = String::from_utf8_lossy(&cd[i + 46..i + 46 + n]).into_owned();
        entries.insert(name, Entry { method, crc, csize, usize, offset });
        i += 46 + n + e + c;
    }
    for m in members {
        let e = entries.get(m).ok_or_else(|| anyhow!("{m} is not in {}", zip.display()))?;
        let mut lh = [0u8; 30];
        f.seek(SeekFrom::Start(e.offset))?;
        f.read_exact(&mut lh)?;
        if lh[..4] != [0x50, 0x4b, 0x03, 0x04] {
            bail!("{m}: a damaged zip entry");
        }
        let data = e.offset + 30 + u16at(&lh, 26) as u64 + u16at(&lh, 28) as u64;
        f.seek(SeekFrom::Start(data))?;
        let raw = (&mut f).take(e.csize);
        let mut src: Box<dyn Read> = match e.method {
            0 => Box::new(raw),
            8 => Box::new(flate2::read::DeflateDecoder::new(raw)),
            other => bail!("{m}: unsupported compression method {other}"),
        };
        let dest = dir.join(file_name(m));
        let tmp = part_path(&dest);
        let mut out = File::create(&tmp)?;
        let mut crc = crc32fast::Hasher::new();
        let mut n = 0u64;
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let k = src.read(&mut buf)?;
            if k == 0 {
                break;
            }
            crc.update(&buf[..k]);
            out.write_all(&buf[..k])?;
            n += k as u64;
        }
        out.sync_all()?;
        drop(out);
        if n != e.usize || crc.finalize() != e.crc {
            let _ = fs::remove_file(&tmp);
            bail!("{m}: the unpacked file does not check out");
        }
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755))?;
        fs::rename(&tmp, &dest)?;
    }
    Ok(())
}

/// Bytes used under `dir`.
pub fn disk_usage(dir: &Path) -> u64 {
    let Ok(rd) = fs::read_dir(dir) else { return 0 };
    rd.flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => disk_usage(&e.path()),
            Ok(t) if t.is_file() => e.metadata().map(|m| m.len()).unwrap_or(0),
            _ => 0,
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A zip with one stored and one deflated member, as Python's zipfile writes them.
    fn zip_with(dir: &Path) -> PathBuf {
        let a = b"stored bytes".to_vec();
        let b: Vec<u8> = (0..5000u32).flat_map(|i| (i % 251).to_le_bytes()).collect();
        let bz = {
            let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(&b).unwrap();
            e.finish().unwrap()
        };
        let mut z = Vec::new();
        let mut cd = Vec::new();
        for (name, method, data, raw) in [("pkg/lib/liba.so.1", 0u16, &a, &a), ("pkg/lib/libb.so.2", 8u16, &bz, &b)] {
            let off = z.len() as u32;
            let crc = crc32fast::hash(raw);
            let mut h = vec![0x50, 0x4b, 0x03, 0x04, 20, 0, 0, 0];
            h.extend_from_slice(&method.to_le_bytes());
            h.extend_from_slice(&[0, 0, 0, 0]);
            h.extend_from_slice(&crc.to_le_bytes());
            h.extend_from_slice(&(data.len() as u32).to_le_bytes());
            h.extend_from_slice(&(raw.len() as u32).to_le_bytes());
            h.extend_from_slice(&(name.len() as u16).to_le_bytes());
            h.extend_from_slice(&0u16.to_le_bytes());
            h.extend_from_slice(name.as_bytes());
            z.extend_from_slice(&h);
            z.extend_from_slice(data);
            let mut c = vec![0x50, 0x4b, 0x01, 0x02, 20, 0, 20, 0, 0, 0];
            c.extend_from_slice(&method.to_le_bytes());
            c.extend_from_slice(&[0, 0, 0, 0]);
            c.extend_from_slice(&crc.to_le_bytes());
            c.extend_from_slice(&(data.len() as u32).to_le_bytes());
            c.extend_from_slice(&(raw.len() as u32).to_le_bytes());
            c.extend_from_slice(&(name.len() as u16).to_le_bytes());
            c.extend_from_slice(&[0; 12]);
            c.extend_from_slice(&off.to_le_bytes());
            c.extend_from_slice(name.as_bytes());
            cd.extend_from_slice(&c);
        }
        let cd_off = z.len() as u32;
        z.extend_from_slice(&cd);
        z.extend_from_slice(&[0x50, 0x4b, 0x05, 0x06, 0, 0, 0, 0, 2, 0, 2, 0]);
        z.extend_from_slice(&(cd.len() as u32).to_le_bytes());
        z.extend_from_slice(&cd_off.to_le_bytes());
        z.extend_from_slice(&[0, 0]);
        let p = dir.join("t.whl");
        fs::write(&p, z).unwrap();
        p
    }

    #[test]
    fn wheels_unpack_and_check() {
        let d = tempfile::tempdir().unwrap();
        let z = zip_with(d.path());
        let out = d.path().join("out");
        fs::create_dir_all(&out).unwrap();
        extract(&z, &["pkg/lib/liba.so.1".into(), "pkg/lib/libb.so.2".into()], &out).unwrap();
        assert_eq!(fs::read(out.join("liba.so.1")).unwrap(), b"stored bytes");
        assert_eq!(fs::read(out.join("libb.so.2")).unwrap().len(), 20000);
        assert!(extract(&z, &["missing".into()], &out).is_err());
        // A flipped byte in the deflated member fails its CRC.
        let mut bytes = fs::read(&z).unwrap();
        let at = bytes.windows(9).position(|w| w == b"libb.so.2").unwrap() + 9 + 40;
        bytes[at] ^= 0x55;
        fs::write(&z, bytes).unwrap();
        assert!(extract(&z, &["pkg/lib/libb.so.2".into()], &out).is_err());
    }
}
