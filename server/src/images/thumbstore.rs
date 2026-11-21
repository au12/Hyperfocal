//! The thumbnail cache as a few large append-only pack files instead of one file per
//! photo. 300,000 thumbnails as 300,000 files make every appdata backup crawl and a cold
//! start slow; as packs they are a handful of big files that copy at disk speed.
//!
//! Layout, in `cache/thumbs/packs/`:
//! - `NNNNNN.pack`: records appended one after another, each a 32-byte header (magic,
//!   kind, photo id, version, length, CRC-32 of the data) and the JPEG. A record is never
//!   changed once written; a new version of a thumbnail is a new record.
//! - `NNNNNN.hint`: for a sealed (full) pack, where its records are, so start-up reads a
//!   few megabytes of hints instead of seeking through gigabytes of JPEGs.
//!
//! In memory, a map from photo (and kind: thumbnail or HEIC draft) to the newest record.
//! Reads are one positioned read (`pread`) and a CRC check; writes append under a lock.
//! Superseded versions, drafts that were replaced and thumbnails of photos that left the
//! index become garbage: `gc` forgets them, and `compact` rewrites a sealed pack once most
//! of it is garbage and deletes the old file. A pack being compacted can still be read
//! by requests that started before (the file stays open until they finish).
//!
//! Nothing here is precious: the whole cache can be deleted at any time and is rebuilt.
//! A record cut short by a crash is dropped at start-up (the active pack is truncated
//! there), and one damaged later fails its CRC on read and is made again.

use anyhow::{Context, Result, anyhow};
use parking_lot::{Mutex, RwLock};
use std::collections::{BTreeMap, HashMap};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const MAGIC: u32 = 0x4b54_4648; // "HFTK"
const HINT_MAGIC: u32 = 0x4e48_4648; // "HFHN"
const HEADER: u64 = 32;
const HINT_REC: usize = 32;
/// A pack is sealed at this size (and a new one started).
pub const PACK_MAX: u64 = 512 << 20;
/// A sealed pack is rewritten once less than this share of it is live.
const COMPACT_BELOW: f64 = 0.5;
/// Records larger than this are refused (a thumbnail is ~30-80 KB).
const MAX_RECORD: u32 = 16 << 20;

/// What a record holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Thumb = 0,
    /// A HEIC file's embedded thumbnail, shown while the full-quality one is made.
    Draft = 1,
}

impl Kind {
    fn from_u8(v: u8) -> Option<Kind> {
        match v {
            0 => Some(Kind::Thumb),
            1 => Some(Kind::Draft),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Loc {
    sig: u64,
    pack: u32,
    off: u64,
    len: u32,
}

struct Pack {
    file: Arc<File>,
    /// Bytes written (the file's length).
    size: u64,
    /// Bytes of records still in the map (headers included).
    live: u64,
    sealed: bool,
}

struct Writer {
    pack: u32,
    file: Arc<File>,
    off: u64,
}

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct Stats {
    pub entries: usize,
    pub packs: usize,
    pub bytes: u64,
    pub live_bytes: u64,
    /// Thumbnail files of the old layout still to move into packs.
    pub legacy_files: usize,
}

pub struct ThumbStore {
    dir: PathBuf,
    /// The old layout (`thumbs/xx/<id>_<v>.jpg`), read while it is being migrated.
    legacy_dir: PathBuf,
    map: RwLock<HashMap<(i64, Kind), Loc>>,
    packs: RwLock<BTreeMap<u32, Pack>>,
    writer: Mutex<Option<Writer>>,
    /// Old-layout files left to migrate (0 when done).
    legacy: std::sync::atomic::AtomicUsize,
    pack_max: u64,
}

fn header(kind: Kind, id: i64, sig: u64, len: u32, crc: u32) -> [u8; HEADER as usize] {
    let mut h = [0u8; HEADER as usize];
    h[0..4].copy_from_slice(&MAGIC.to_le_bytes());
    h[4] = kind as u8;
    h[8..16].copy_from_slice(&id.to_le_bytes());
    h[16..24].copy_from_slice(&sig.to_le_bytes());
    h[24..28].copy_from_slice(&len.to_le_bytes());
    h[28..32].copy_from_slice(&crc.to_le_bytes());
    h
}

fn parse_header(h: &[u8]) -> Option<(Kind, i64, u64, u32, u32)> {
    if h.len() < HEADER as usize || u32::from_le_bytes(h[0..4].try_into().ok()?) != MAGIC {
        return None;
    }
    let kind = Kind::from_u8(h[4])?;
    let id = i64::from_le_bytes(h[8..16].try_into().ok()?);
    let sig = u64::from_le_bytes(h[16..24].try_into().ok()?);
    let len = u32::from_le_bytes(h[24..28].try_into().ok()?);
    let crc = u32::from_le_bytes(h[28..32].try_into().ok()?);
    (len <= MAX_RECORD).then_some((kind, id, sig, len, crc))
}

fn pack_name(n: u32) -> String {
    format!("{n:06}.pack")
}

fn hint_name(n: u32) -> String {
    format!("{n:06}.hint")
}

/// Don't let the kernel read ahead through a pack being scanned or read at random.
fn advise_random(f: &File) {
    use std::os::fd::AsRawFd;
    // SAFETY: a valid descriptor; advice only.
    unsafe {
        libc::posix_fadvise(f.as_raw_fd(), 0, 0, libc::POSIX_FADV_RANDOM);
    }
}

impl ThumbStore {
    /// Opens (or creates) the store in `thumbs_dir/packs`, reading the hints of sealed
    /// packs and scanning the active one.
    pub fn open(thumbs_dir: &Path) -> Result<ThumbStore> {
        Self::open_with(thumbs_dir, PACK_MAX)
    }

    pub fn open_with(thumbs_dir: &Path, pack_max: u64) -> Result<ThumbStore> {
        let dir = thumbs_dir.join("packs");
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        let s = ThumbStore {
            dir,
            legacy_dir: thumbs_dir.to_path_buf(),
            map: RwLock::new(HashMap::new()),
            packs: RwLock::new(BTreeMap::new()),
            writer: Mutex::new(None),
            legacy: std::sync::atomic::AtomicUsize::new(0),
            pack_max: pack_max.max(HEADER * 4),
        };
        s.load()?;
        s.legacy.store(s.count_legacy(), std::sync::atomic::Ordering::Relaxed);
        Ok(s)
    }

    fn load(&self) -> Result<()> {
        let mut numbers: Vec<u32> = std::fs::read_dir(&self.dir)?
            .flatten()
            .filter_map(|e| e.file_name().to_str().and_then(|n| n.strip_suffix(".pack")).and_then(|n| n.parse().ok()))
            .collect();
        numbers.sort_unstable();
        let mut map = HashMap::new();
        let mut packs = BTreeMap::new();
        let mut rehint = Vec::new();
        let last = numbers.last().copied();
        for &n in &numbers {
            let path = self.dir.join(pack_name(n));
            let file = OpenOptions::new().read(true).write(true).open(&path).with_context(|| format!("opening {}", path.display()))?;
            advise_random(&file);
            let size = file.metadata()?.len();
            let hinted = Some(n) != last && self.read_hint(n, size, &mut map).is_ok();
            let end = if hinted { size } else { scan(&file, n, size, &mut map)? };
            if end < size {
                // A record cut short (a crash while appending): drop it.
                tracing::warn!("thumbnail pack {}: dropping {} bytes of an unfinished record", path.display(), size - end);
                file.set_len(end)?;
            }
            packs.insert(n, Pack { file: Arc::new(file), size: end, live: 0, sealed: Some(n) != last || end >= self.pack_max });
            if !hinted && Some(n) != last {
                rehint.push((n, end));
            }
        }
        for loc in map.values() {
            if let Some(p) = packs.get_mut(&loc.pack) {
                p.live += HEADER + loc.len as u64;
            }
        }
        *self.map.write() = map;
        *self.packs.write() = packs;
        // Sealed packs whose hints were missing or stale: write them again.
        for (n, size) in rehint {
            if let Err(e) = self.write_hint(n, size) {
                tracing::warn!("thumbnail pack {n}: hints not written ({e:#})");
            }
        }
        Ok(())
    }

    fn read_hint(&self, n: u32, size: u64, map: &mut HashMap<(i64, Kind), Loc>) -> Result<()> {
        let data = std::fs::read(self.dir.join(hint_name(n)))?;
        if data.len() < 16 || u32::from_le_bytes(data[0..4].try_into()?) != HINT_MAGIC {
            return Err(anyhow!("not a hint file"));
        }
        if u64::from_le_bytes(data[4..12].try_into()?) != size {
            return Err(anyhow!("the pack changed since its hints were written"));
        }
        let count = u32::from_le_bytes(data[12..16].try_into()?) as usize;
        if data.len() != 16 + count * HINT_REC {
            return Err(anyhow!("truncated hint file"));
        }
        for r in data[16..].chunks_exact(HINT_REC) {
            let kind = Kind::from_u8(r[0]).ok_or_else(|| anyhow!("bad kind"))?;
            let len = u32::from_le_bytes(r[1..5].try_into()?);
            let id = i64::from_le_bytes(r[5..13].try_into()?);
            let sig = u64::from_le_bytes(r[13..21].try_into()?);
            let off = u64::from_le_bytes(r[21..29].try_into()?);
            if off + HEADER + len as u64 > size || len > MAX_RECORD {
                return Err(anyhow!("hint past the end of the pack"));
            }
            map.insert((id, kind), Loc { sig, pack: n, off, len });
        }
        Ok(())
    }

    fn write_hint(&self, n: u32, size: u64) -> Result<()> {
        let recs: Vec<((i64, Kind), Loc)> = self.map.read().iter().filter(|(_, l)| l.pack == n).map(|(k, l)| (*k, *l)).collect();
        let mut out = Vec::with_capacity(16 + recs.len() * HINT_REC);
        out.extend_from_slice(&HINT_MAGIC.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&(recs.len() as u32).to_le_bytes());
        for ((id, kind), l) in recs {
            out.push(kind as u8);
            out.extend_from_slice(&l.len.to_le_bytes());
            out.extend_from_slice(&id.to_le_bytes());
            out.extend_from_slice(&l.sig.to_le_bytes());
            out.extend_from_slice(&l.off.to_le_bytes());
            out.extend_from_slice(&[0u8; 3]);
        }
        let tmp = self.dir.join(format!("{}.tmp", hint_name(n)));
        {
            let mut f = File::create(&tmp)?;
            f.write_all(&out)?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, self.dir.join(hint_name(n)))?;
        Ok(())
    }

    /// The thumbnail (or draft) of photo `id` at version `sig`, if stored.
    pub fn get(&self, id: i64, sig: u64, kind: Kind) -> Option<Vec<u8>> {
        for _ in 0..2 {
            let loc = *self.map.read().get(&(id, kind)).filter(|l| l.sig == sig)?;
            match self.read(&loc) {
                Ok(b) => return Some(b),
                Err(e) => {
                    let mut m = self.map.write();
                    if m.get(&(id, kind)) != Some(&loc) {
                        // Moved by a compaction meanwhile: read it where it is now.
                        continue;
                    }
                    tracing::debug!("thumbnail {id} unreadable in its pack ({e:#}); making it again");
                    m.remove(&(id, kind));
                    drop(m);
                    self.forget_bytes(&loc);
                    return None;
                }
            }
        }
        None
    }

    /// Whether a thumbnail (or draft) of photo `id` at version `sig` is stored.
    pub fn contains(&self, id: i64, sig: u64, kind: Kind) -> bool {
        self.map.read().get(&(id, kind)).is_some_and(|l| l.sig == sig)
    }

    fn read(&self, loc: &Loc) -> Result<Vec<u8>> {
        let file = self.packs.read().get(&loc.pack).map(|p| p.file.clone()).ok_or_else(|| anyhow!("pack {} is gone", loc.pack))?;
        let mut buf = vec![0u8; (HEADER + loc.len as u64) as usize];
        file.read_exact_at(&mut buf, loc.off)?;
        let (_, _, sig, len, crc) = parse_header(&buf).ok_or_else(|| anyhow!("bad record header"))?;
        if sig != loc.sig || len != loc.len {
            return Err(anyhow!("record does not match the index"));
        }
        let data = buf.split_off(HEADER as usize);
        if crc32fast::hash(&data) != crc {
            return Err(anyhow!("checksum mismatch"));
        }
        Ok(data)
    }

    /// Appends a record to the active pack (rolling to a new one when it is full); the
    /// caller holds the writer lock. Returns where it went.
    fn append(&self, w: &mut Option<Writer>, kind: Kind, id: i64, sig: u64, data: &[u8]) -> Result<Loc> {
        let need = HEADER + data.len() as u64;
        if w.as_ref().is_none_or(|w| w.off + need > self.pack_max && w.off > 0) {
            self.roll(w)?;
        }
        let wr = w.as_mut().expect("writer");
        // One write for header and data: a record is either all there or cut short at the
        // end of the pack, which start-up drops.
        let mut rec = Vec::with_capacity(need as usize);
        rec.extend_from_slice(&header(kind, id, sig, data.len() as u32, crc32fast::hash(data)));
        rec.extend_from_slice(data);
        wr.file.write_all_at(&rec, wr.off)?;
        let loc = Loc { sig, pack: wr.pack, off: wr.off, len: data.len() as u32 };
        wr.off += need;
        if let Some(p) = self.packs.write().get_mut(&wr.pack) {
            p.size = wr.off;
            p.live += need;
        }
        Ok(loc)
    }

    /// Stores a thumbnail (or draft) of photo `id` at version `sig`, replacing any other
    /// version of it.
    pub fn put(&self, id: i64, sig: u64, kind: Kind, data: &[u8]) -> Result<()> {
        if data.is_empty() || data.len() as u64 > MAX_RECORD as u64 {
            return Err(anyhow!("thumbnail of {} bytes", data.len()));
        }
        let mut w = self.writer.lock();
        let loc = self.append(&mut w, kind, id, sig, data)?;
        let old = self.map.write().insert((id, kind), loc);
        drop(w);
        if let Some(old) = old {
            self.forget_bytes(&old);
        }
        Ok(())
    }

    /// Forgets a thumbnail (or draft): its bytes become garbage for `compact`.
    pub fn remove(&self, id: i64, kind: Kind) {
        let old = self.map.write().remove(&(id, kind));
        if let Some(old) = old {
            self.forget_bytes(&old);
        }
    }

    fn forget_bytes(&self, loc: &Loc) {
        if let Some(p) = self.packs.write().get_mut(&loc.pack) {
            p.live = p.live.saturating_sub(HEADER + loc.len as u64);
        }
    }

    /// Seals the active pack (writing its hints) and starts a new one.
    fn roll(&self, w: &mut Option<Writer>) -> Result<()> {
        if let Some(cur) = w.take() {
            cur.file.sync_data()?;
            if let Err(e) = self.write_hint(cur.pack, cur.off) {
                tracing::warn!("thumbnail pack {}: hints not written ({e:#})", cur.pack);
            }
            if let Some(p) = self.packs.write().get_mut(&cur.pack) {
                p.sealed = true;
            }
        }
        // Continue the newest pack if it still has room (after a restart), else a new one.
        let (n, reuse) = {
            let packs = self.packs.read();
            match packs.iter().next_back() {
                Some((&n, p)) if !p.sealed && p.size < self.pack_max => (n, Some((p.file.clone(), p.size))),
                Some((&n, _)) => (n + 1, None),
                None => (1, None),
            }
        };
        let (file, off) = match reuse {
            Some(x) => x,
            None => {
                let path = self.dir.join(pack_name(n));
                let f = OpenOptions::new().read(true).write(true).create_new(true).open(&path).with_context(|| format!("creating {}", path.display()))?;
                advise_random(&f);
                let f = Arc::new(f);
                self.packs.write().insert(n, Pack { file: f.clone(), size: 0, live: 0, sealed: false });
                (f, 0)
            }
        };
        *w = Some(Writer { pack: n, file, off });
        Ok(())
    }

    /// Forgets every thumbnail `keep` says no longer belongs to a photo of the index (or
    /// is of another version than the photo's own), and drafts whose full-quality
    /// thumbnail is there. Returns how many it forgot.
    pub fn gc(&self, keep: impl Fn(i64, u64) -> bool) -> usize {
        let snap: Vec<((i64, Kind), Loc)> = self.map.read().iter().map(|(k, l)| (*k, *l)).collect();
        let thumbs: HashMap<i64, u64> = snap.iter().filter(|((_, k), _)| *k == Kind::Thumb).map(|((id, _), l)| (*id, l.sig)).collect();
        let mut n = 0;
        for ((id, kind), loc) in snap {
            let stale = !keep(id, loc.sig) || (kind == Kind::Draft && thumbs.get(&id) == Some(&loc.sig));
            if !stale {
                continue;
            }
            let removed = {
                let mut m = self.map.write();
                if m.get(&(id, kind)) == Some(&loc) {
                    m.remove(&(id, kind));
                    true
                } else {
                    false
                }
            };
            if removed {
                self.forget_bytes(&loc);
                n += 1;
            }
        }
        n
    }

    /// Rewrites the sealed pack with the least live data, if under half of it is live:
    /// its live records go to the active pack and the old file is deleted. `yield_now`
    /// is called between records (to let interactive work go first). Returns the bytes
    /// freed.
    pub fn compact(&self, yield_now: &dyn Fn()) -> Result<u64> {
        let pick = {
            let packs = self.packs.read();
            packs
                .iter()
                .filter(|(_, p)| p.sealed && p.size > 0 && (p.live as f64) < p.size as f64 * COMPACT_BELOW)
                .min_by(|a, b| (a.1.live as f64 / a.1.size as f64).total_cmp(&(b.1.live as f64 / b.1.size as f64)))
                .map(|(&n, p)| (n, p.size))
        };
        let Some((n, size)) = pick else { return Ok(0) };
        let recs: Vec<((i64, Kind), Loc)> = self.map.read().iter().filter(|(_, l)| l.pack == n).map(|(k, l)| (*k, *l)).collect();
        for ((id, kind), loc) in recs {
            yield_now();
            // Moved or replaced meanwhile: nothing to copy.
            if self.map.read().get(&(id, kind)) != Some(&loc) {
                continue;
            }
            match self.read(&loc) {
                Ok(data) => {
                    // `put` replaces the entry only if it is still this record.
                    self.put_if(id, kind, &loc, &data)?;
                }
                Err(_) => self.remove(id, kind),
            }
        }
        let still: usize = self.map.read().values().filter(|l| l.pack == n).count();
        if still > 0 {
            return Err(anyhow!("pack {n} still holds {still} records"));
        }
        self.packs.write().remove(&n);
        let _ = std::fs::remove_file(self.dir.join(hint_name(n)));
        std::fs::remove_file(self.dir.join(pack_name(n)))?;
        Ok(size)
    }

    /// `put`, but only while the entry is still `expect` (compaction must not bring back
    /// a version replaced meanwhile).
    fn put_if(&self, id: i64, kind: Kind, expect: &Loc, data: &[u8]) -> Result<()> {
        let mut w = self.writer.lock();
        if self.map.read().get(&(id, kind)) != Some(expect) {
            return Ok(());
        }
        let loc = self.append(&mut w, kind, id, expect.sig, data)?;
        self.map.write().insert((id, kind), loc);
        drop(w);
        self.forget_bytes(expect);
        Ok(())
    }

    pub fn stats(&self) -> Stats {
        let packs = self.packs.read();
        Stats {
            entries: self.map.read().len(),
            packs: packs.len(),
            bytes: packs.values().map(|p| p.size).sum(),
            live_bytes: packs.values().map(|p| p.live).sum(),
            legacy_files: self.legacy.load(std::sync::atomic::Ordering::Relaxed),
        }
    }

    // ------------------------------------------------------------ the old layout

    /// Where the old layout kept a photo's thumbnail.
    pub fn legacy_path(&self, id: i64, v: &str) -> PathBuf {
        self.legacy_dir.join(format!("{:02x}", id.rem_euclid(256))).join(format!("{id}_{v}.jpg"))
    }

    fn legacy_dirs(&self) -> Vec<PathBuf> {
        let Ok(rd) = std::fs::read_dir(&self.legacy_dir) else { return Vec::new() };
        rd.flatten()
            .filter(|e| {
                let n = e.file_name();
                let n = n.to_string_lossy();
                n.len() == 2 && n.bytes().all(|b| b.is_ascii_hexdigit()) && e.file_type().is_ok_and(|t| t.is_dir())
            })
            .map(|e| e.path())
            .collect()
    }

    fn count_legacy(&self) -> usize {
        self.legacy_dirs().iter().filter_map(|d| std::fs::read_dir(d).ok()).map(|rd| rd.count()).sum()
    }

    /// Whether thumbnails of the old layout are left to migrate.
    pub fn has_legacy(&self) -> bool {
        self.legacy.load(std::sync::atomic::Ordering::Relaxed) > 0
    }

    /// Moves the thumbnails of the old layout into packs and deletes their files (drafts
    /// and leftovers are just deleted: they are made again when needed). `keep` says
    /// whether a thumbnail still belongs to a photo of the index; `yield_now` is called
    /// between files. Returns how many thumbnails moved.
    pub fn migrate_legacy(&self, keep: &dyn Fn(i64, u64) -> bool, yield_now: &dyn Fn()) -> Result<usize> {
        let mut moved = 0;
        for dir in self.legacy_dirs() {
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            for e in rd.flatten() {
                yield_now();
                let path = e.path();
                let name = e.file_name().to_string_lossy().into_owned();
                let parsed = name.strip_suffix(".jpg").filter(|s| !s.contains('.')).and_then(|s| s.split_once('_')).and_then(|(a, b)| {
                    let id = a.parse::<i64>().ok()?;
                    let sig = u64::from_str_radix(b, 16).ok()?;
                    Some((id, sig))
                });
                if let Some((id, sig)) = parsed
                    && keep(id, sig)
                    && !self.contains(id, sig, Kind::Thumb)
                    && let Ok(data) = std::fs::read(&path)
                    && !data.is_empty()
                {
                    self.put(id, sig, Kind::Thumb, &data)?;
                    moved += 1;
                }
                let _ = std::fs::remove_file(&path);
                let _ = self.legacy.fetch_update(std::sync::atomic::Ordering::Relaxed, std::sync::atomic::Ordering::Relaxed, |v| Some(v.saturating_sub(1)));
            }
            let _ = std::fs::remove_dir(&dir);
        }
        self.legacy.store(self.count_legacy(), std::sync::atomic::Ordering::Relaxed);
        Ok(moved)
    }
}

/// Reads the record headers of a pack from the start. Returns where the last complete
/// record ends.
fn scan(file: &File, n: u32, size: u64, map: &mut HashMap<(i64, Kind), Loc>) -> Result<u64> {
    let mut off = 0u64;
    let mut h = [0u8; HEADER as usize];
    while off + HEADER <= size {
        if file.read_exact_at(&mut h, off).is_err() {
            break;
        }
        let Some((kind, id, sig, len, _)) = parse_header(&h) else { break };
        let end = off + HEADER + len as u64;
        if end > size {
            break;
        }
        map.insert((id, kind), Loc { sig, pack: n, off, len });
        off = end;
    }
    Ok(off)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jpeg(seed: u8, n: usize) -> Vec<u8> {
        (0..n).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed)).collect()
    }

    #[test]
    fn put_get_replace_and_reopen() {
        let t = tempfile::tempdir().unwrap();
        let s = ThumbStore::open_with(t.path(), 4096).unwrap();
        s.put(1, 0xabc, Kind::Thumb, &jpeg(1, 1000)).unwrap();
        s.put(2, 0xdef, Kind::Thumb, &jpeg(2, 1000)).unwrap();
        s.put(2, 0xdef, Kind::Draft, &jpeg(3, 300)).unwrap();
        assert_eq!(s.get(1, 0xabc, Kind::Thumb).unwrap(), jpeg(1, 1000));
        assert!(s.get(1, 0xabd, Kind::Thumb).is_none(), "another version");
        // A new version replaces the old one.
        s.put(1, 0x111, Kind::Thumb, &jpeg(4, 1200)).unwrap();
        assert!(s.get(1, 0xabc, Kind::Thumb).is_none());
        assert_eq!(s.get(1, 0x111, Kind::Thumb).unwrap(), jpeg(4, 1200));
        // Enough to fill several packs (4 KB each).
        for i in 10..40 {
            s.put(i, i as u64, Kind::Thumb, &jpeg(i as u8, 900)).unwrap();
        }
        let st = s.stats();
        assert!(st.packs > 3, "{st:?}");
        drop(s);
        // Reopened: from hints for the sealed packs, a scan of the last.
        let s = ThumbStore::open_with(t.path(), 4096).unwrap();
        assert_eq!(s.get(1, 0x111, Kind::Thumb).unwrap(), jpeg(4, 1200));
        assert_eq!(s.get(2, 0xdef, Kind::Draft).unwrap(), jpeg(3, 300));
        for i in 10..40 {
            assert_eq!(s.get(i, i as u64, Kind::Thumb).unwrap(), jpeg(i as u8, 900), "{i}");
        }
        assert_eq!(s.stats().entries, 33);
    }

    #[test]
    fn torn_record_is_dropped_and_damage_detected() {
        let t = tempfile::tempdir().unwrap();
        {
            let s = ThumbStore::open_with(t.path(), 1 << 20).unwrap();
            s.put(1, 1, Kind::Thumb, &jpeg(1, 500)).unwrap();
            s.put(2, 2, Kind::Thumb, &jpeg(2, 500)).unwrap();
        }
        let pack = t.path().join("packs").join(pack_name(1));
        let full = std::fs::metadata(&pack).unwrap().len();
        // A crash in the middle of the second record.
        let f = OpenOptions::new().write(true).open(&pack).unwrap();
        f.set_len(full - 100).unwrap();
        drop(f);
        let s = ThumbStore::open_with(t.path(), 1 << 20).unwrap();
        assert!(s.get(1, 1, Kind::Thumb).is_some());
        assert!(s.get(2, 2, Kind::Thumb).is_none());
        assert_eq!(std::fs::metadata(&pack).unwrap().len(), HEADER + 500);
        // Writing goes on after the last complete record.
        s.put(3, 3, Kind::Thumb, &jpeg(3, 500)).unwrap();
        assert_eq!(s.get(3, 3, Kind::Thumb).unwrap(), jpeg(3, 500));
        // A flipped byte in the data fails the checksum: the entry is dropped.
        let f = OpenOptions::new().write(true).open(&pack).unwrap();
        f.write_all_at(&[0xff], HEADER + 10).unwrap();
        drop(f);
        assert!(s.get(1, 1, Kind::Thumb).is_none());
        assert!(!s.contains(1, 1, Kind::Thumb));
    }

    #[test]
    fn gc_and_compaction_reclaim_space() {
        let t = tempfile::tempdir().unwrap();
        let s = ThumbStore::open_with(t.path(), 8192).unwrap();
        for i in 0..40 {
            s.put(i, 1, Kind::Thumb, &jpeg(i as u8, 1000)).unwrap();
        }
        let before = s.stats();
        // Photos 0..30 left the index; 30..40 changed version.
        let n = s.gc(|id, sig| id >= 30 && sig == 1);
        assert_eq!(n, 30);
        for i in 30..40 {
            s.put(i, 2, Kind::Thumb, &jpeg(100 + i as u8, 1000)).unwrap();
        }
        assert_eq!(s.gc(|_, sig| sig == 2), 0, "only the new versions are live");
        let mut freed = 0;
        loop {
            let f = s.compact(&|| {}).unwrap();
            if f == 0 {
                break;
            }
            freed += f;
        }
        let after = s.stats();
        assert!(freed > 0 && after.bytes < before.bytes, "{before:?} -> {after:?}");
        for i in 30..40 {
            assert_eq!(s.get(i, 2, Kind::Thumb).unwrap(), jpeg(100 + i as u8, 1000));
        }
        assert_eq!(after.entries, 10);
        drop(s);
        let s = ThumbStore::open_with(t.path(), 8192).unwrap();
        for i in 30..40 {
            assert_eq!(s.get(i, 2, Kind::Thumb).unwrap(), jpeg(100 + i as u8, 1000));
        }
    }

    #[test]
    fn migrates_the_old_layout() {
        let t = tempfile::tempdir().unwrap();
        for (id, v) in [(5i64, "1a2b"), (261, "ff00"), (7, "77")] {
            let d = t.path().join(format!("{:02x}", id % 256));
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join(format!("{id}_{v}.jpg")), jpeg(id as u8, 400)).unwrap();
        }
        std::fs::write(t.path().join("05").join("5_1a2b.draft.jpg"), b"draft").unwrap();
        let s = ThumbStore::open(t.path()).unwrap();
        assert!(s.has_legacy());
        assert_eq!(s.stats().legacy_files, 4);
        // Photo 7 left the index.
        let moved = s.migrate_legacy(&|id, _| id != 7, &|| {}).unwrap();
        assert_eq!(moved, 2);
        assert!(!s.has_legacy());
        assert_eq!(s.get(5, 0x1a2b, Kind::Thumb).unwrap(), jpeg(5, 400));
        assert_eq!(s.get(261, 0xff00, Kind::Thumb).unwrap(), jpeg(261u16 as u8, 400));
        assert!(s.get(7, 0x77, Kind::Thumb).is_none());
        assert!(!t.path().join("05").exists(), "old folders are removed");
        assert!(t.path().join("packs").exists());
    }
}
