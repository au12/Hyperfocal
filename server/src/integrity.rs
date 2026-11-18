//! Archive integrity monitor: a content hash (xxh3-128) of every original, verified
//! again slowly in the background, and a report of any original that changed, became
//! unreadable or vanished, in the UI and through an optional alert (ntfy, Gotify or
//! any webhook). Report-only: nothing here writes to the photo library.
//!
//! JPEG, HEIF, TIFF and DNG files are formats apps write metadata into, so for them
//! the hash covers only the image data: a metadata edit is not reported as damage.
//!
//! When it reads files:
//! - soon after a scan notices them (a few GB an hour at most): fresh imports,
//!   originals whose size or date changed, files that vanished (to tell a move from a
//!   loss), files that came back, and rechecks asked for in the UI;
//! - everything else (the first hash of an existing library, and the re-verification
//!   that comes round every `verify_days`) in a nightly window, paced, so array disks
//!   are not kept spinning all day.

use crate::api::{ApiError, ApiResult, St, bad};
use crate::state::{AppState, Event};
use anyhow::Result;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use parking_lot::{Condvar, Mutex, RwLock};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use xxhash_rust::xxh3::Xxh3;

pub const OK: i64 = 0;
pub const CHANGED: i64 = 1;
pub const MISSING: i64 = 2;
pub const UNREADABLE: i64 = 3;

/// What part of a file a hash covers.
pub const SCOPE_FILE: i64 = 0;
pub const SCOPE_JPEG: i64 = 1;
pub const SCOPE_HEIF: i64 = 2;
pub const SCOPE_TIFF: i64 = 3;

/// Files new to the index within this long are hashed promptly (fresh imports).
const FRESH: i64 = 7 * 86_400_000;
/// Reads allowed per hour outside the nightly window (and its burst).
const HOURLY_BYTES: u64 = 4 << 30;
/// A problem waits this long before an alert, in case it resolves itself (a move
/// still being scanned, a file being rewritten).
const GRACE_MISSING: i64 = 6 * 3_600_000;
const GRACE_OTHER: i64 = 3_600_000;
/// At most one alert an hour.
const ALERT_EVERY: Duration = Duration::from_secs(3600);

pub fn scope_for(ext: &str) -> i64 {
    match ext.to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" | "jpe" => SCOPE_JPEG,
        "heic" | "heif" | "hif" | "avif" => SCOPE_HEIF,
        "tif" | "tiff" | "dng" => SCOPE_TIFF,
        _ => SCOPE_FILE,
    }
}

// ------------------------------------------------------------------ pacing

/// Read pacing: a rate limit, a pause while the viewer is busy, and a stop.
pub struct Pace<'a> {
    rate: f64,
    t0: Instant,
    bytes: u64,
    busy: &'a dyn Fn() -> bool,
    stop: &'a dyn Fn() -> bool,
}

fn never() -> bool {
    false
}

impl<'a> Pace<'a> {
    pub fn new(mbps: u32, busy: &'a dyn Fn() -> bool, stop: &'a dyn Fn() -> bool) -> Pace<'a> {
        Pace { rate: mbps as f64 * 1_048_576.0, t0: Instant::now(), bytes: 0, busy, stop }
    }

    pub fn unlimited() -> Pace<'static> {
        Pace { rate: 0.0, t0: Instant::now(), bytes: 0, busy: &never, stop: &never }
    }

    fn take(&mut self, n: usize) -> io::Result<()> {
        if (self.stop)() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "stopped"));
        }
        if (self.busy)() {
            while (self.busy)() {
                std::thread::sleep(Duration::from_millis(50));
                if (self.stop)() {
                    return Err(io::Error::new(io::ErrorKind::Interrupted, "stopped"));
                }
            }
            // No burst to catch up after a pause.
            self.t0 = Instant::now();
            self.bytes = 0;
        }
        self.bytes += n as u64;
        if self.rate > 0.0 {
            let due = Duration::from_secs_f64(self.bytes as f64 / self.rate);
            let el = self.t0.elapsed();
            if due > el + Duration::from_millis(2) {
                std::thread::sleep(due - el);
            }
        }
        Ok(())
    }
}

/// A file read through the pace.
struct Paced<'a, 'p> {
    f: File,
    pace: &'a mut Pace<'p>,
}

impl Read for Paced<'_, '_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.f.read(buf)?;
        self.pace.take(n)?;
        Ok(n)
    }
}

impl Seek for Paced<'_, '_> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.f.seek(pos)
    }
}

/// Page-cache hints: read ahead now, forget afterwards (hashing a whole library must
/// not push the viewer's files out of memory).
fn advise(f: &File, done: bool) {
    #[cfg(target_os = "linux")]
    unsafe {
        let a = if done { libc::POSIX_FADV_DONTNEED } else { libc::POSIX_FADV_SEQUENTIAL };
        libc::posix_fadvise(std::os::fd::AsRawFd::as_raw_fd(f), 0, 0, a);
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (f, done);
}

// ------------------------------------------------------------------ hashing

fn hex(h: &Xxh3) -> String {
    format!("{:032x}", h.digest128())
}

/// (scope, hash) of a file: its image data for formats that carry editable metadata,
/// else all of it. A file whose image data cannot be found is hashed whole.
pub fn hash_file(path: &Path, pace: &mut Pace) -> io::Result<(i64, String)> {
    let f = File::open(path)?;
    let len = f.metadata()?.len();
    advise(&f, false);
    let want = scope_for(path.extension().and_then(|e| e.to_str()).unwrap_or(""));
    let mut r = Paced { f, pace };
    let res = hash_open(&mut r, len, want);
    advise(&r.f, true);
    res
}

fn hash_open(r: &mut Paced, len: u64, want: i64) -> io::Result<(i64, String)> {
    let mut h = Xxh3::new();
    let found = match want {
        SCOPE_JPEG => jpeg_parts(r, len, &mut h)?,
        SCOPE_HEIF => {
            r.seek(SeekFrom::Start(0))?;
            let ok = crate::meta::heif::image_parts(r, len, &mut |b| h.update(b)).is_ok();
            if !ok && (r.pace.stop)() {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "stopped"));
            }
            ok
        }
        SCOPE_TIFF => tiff_parts(r, len, &mut h)?,
        _ => false,
    };
    if found {
        return Ok((want, hex(&h)));
    }
    let mut h = Xxh3::new();
    r.seek(SeekFrom::Start(0))?;
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok((SCOPE_FILE, hex(&h)))
}

fn jpeg_parts(r: &mut Paced, len: u64, h: &mut Xxh3) -> io::Result<bool> {
    if len > 512 << 20 {
        return Ok(false);
    }
    let mut b = Vec::with_capacity(len as usize);
    r.seek(SeekFrom::Start(0))?;
    r.by_ref().take(len).read_to_end(&mut b)?;
    Ok(jpeg_image(&b, h))
}

/// Feeds a JPEG's image data to `h`: every marker segment except APPn (EXIF, XMP,
/// ICC, IPTC...) and comments, and the entropy-coded data, up to the end of the first
/// image. False when this is not a well-formed JPEG.
pub fn jpeg_image(b: &[u8], h: &mut Xxh3) -> bool {
    if b.len() < 4 || b[0] != 0xFF || b[1] != 0xD8 {
        return false;
    }
    h.update(&b[..2]);
    let mut i = 2;
    loop {
        // At a marker: 0xFF, any fill bytes, the code.
        if b.get(i) != Some(&0xFF) {
            return false;
        }
        while b.get(i) == Some(&0xFF) {
            i += 1;
        }
        let Some(&m) = b.get(i) else { return false };
        i += 1;
        match m {
            0xD9 => {
                h.update(&[0xFF, 0xD9]);
                return true;
            }
            0x01 | 0xD0..=0xD7 => {
                h.update(&[0xFF, m]);
                continue;
            }
            0x00 => return false,
            _ => {}
        }
        let Some(len) = b.get(i..i + 2).map(|l| u16::from_be_bytes([l[0], l[1]]) as usize) else { return false };
        if len < 2 || i + len > b.len() {
            return false;
        }
        if !((0xE0..=0xEF).contains(&m) || m == 0xFE) {
            h.update(&[0xFF, m]);
            h.update(&b[i..i + len]);
        }
        i += len;
        if m == 0xDA {
            // Entropy-coded data runs to the next marker that is not a stuffed 0xFF00
            // or a restart marker.
            let start = i;
            loop {
                let Some(p) = b[i..].iter().position(|&x| x == 0xFF) else { return false };
                let j = i + p;
                match b.get(j + 1) {
                    None => return false,
                    Some(0x00 | 0xD0..=0xD7) => i = j + 2,
                    Some(0xFF) => i = j + 1,
                    Some(_) => {
                        h.update(&b[start..j]);
                        i = j;
                        break;
                    }
                }
            }
        }
    }
}

/// Feeds the main image of a TIFF or DNG file to `h`: its size and its strips or tiles
/// (the raw data of a DNG), never previews or metadata. False when there is none.
fn tiff_parts(r: &mut Paced, len: u64, h: &mut Xxh3) -> io::Result<bool> {
    let mut hd = [0u8; 8];
    r.seek(SeekFrom::Start(0))?;
    if len < 8 {
        return Ok(false);
    }
    r.read_exact(&mut hd)?;
    let le = match &hd[..4] {
        b"II*\0" => true,
        b"MM\0*" => false,
        _ => return Ok(false),
    };
    let u16_ = |b: &[u8]| if le { u16::from_le_bytes([b[0], b[1]]) } else { u16::from_be_bytes([b[0], b[1]]) };
    let u32_ = |b: &[u8]| if le { u32::from_le_bytes([b[0], b[1], b[2], b[3]]) } else { u32::from_be_bytes([b[0], b[1], b[2], b[3]]) };

    #[derive(Default)]
    struct Img {
        w: u64,
        h: u64,
        sub: u64,
        offs: Vec<u64>,
        counts: Vec<u64>,
    }
    let mut imgs: Vec<Img> = Vec::new();
    let mut queue = vec![u32_(&hd[4..8]) as u64];
    let mut seen = std::collections::HashSet::new();
    while let Some(off) = queue.pop() {
        if off < 8 || off + 2 > len || !seen.insert(off) || seen.len() > 64 {
            continue;
        }
        let mut cnt = [0u8; 2];
        r.seek(SeekFrom::Start(off))?;
        r.read_exact(&mut cnt)?;
        let n = u16_(&cnt) as u64;
        if n == 0 || n > 4096 || off + 2 + n * 12 + 4 > len {
            continue;
        }
        let mut ents = vec![0u8; (n * 12 + 4) as usize];
        r.read_exact(&mut ents)?;
        queue.push(u32_(&ents[(n * 12) as usize..]) as u64);
        let mut img = Img::default();
        for e in ents[..(n * 12) as usize].chunks_exact(12) {
            let (tag, typ, count) = (u16_(&e[0..2]), u16_(&e[2..4]), u32_(&e[4..8]) as u64);
            if !matches!(tag, 254 | 256 | 257 | 273 | 279 | 324 | 325 | 330) {
                continue;
            }
            let size = match typ {
                3 => 2,
                4 | 13 => 4,
                _ => continue,
            };
            if count == 0 || count > 1 << 22 {
                continue;
            }
            let raw = if count * size <= 4 {
                e[8..12].to_vec()
            } else {
                let at = u32_(&e[8..12]) as u64;
                if at + count * size > len {
                    continue;
                }
                let mut v = vec![0u8; (count * size) as usize];
                r.seek(SeekFrom::Start(at))?;
                r.read_exact(&mut v)?;
                v
            };
            let vals: Vec<u64> = raw.chunks_exact(size as usize).take(count as usize).map(|c| if size == 2 { u16_(c) as u64 } else { u32_(c) as u64 }).collect();
            match tag {
                254 => img.sub = vals[0],
                256 => img.w = vals[0],
                257 => img.h = vals[0],
                273 | 324 => img.offs = vals,
                279 | 325 => img.counts = vals,
                330 => queue.extend(vals),
                _ => {}
            }
        }
        if !img.offs.is_empty() && img.offs.len() == img.counts.len() && img.w > 0 && img.h > 0 {
            imgs.push(img);
        }
    }
    // The full-resolution image: not marked reduced-resolution, the largest.
    let Some(m) = imgs.iter().filter(|i| i.sub & 1 == 0).max_by_key(|i| i.w * i.h) else { return Ok(false) };
    if m.offs.iter().zip(&m.counts).any(|(o, n)| o + n > len) {
        return Ok(false);
    }
    h.update(&m.w.to_le_bytes());
    h.update(&m.h.to_le_bytes());
    let mut buf = vec![0u8; 1 << 20];
    for (&o, &n) in m.offs.iter().zip(&m.counts) {
        r.seek(SeekFrom::Start(o))?;
        let mut left = n;
        while left > 0 {
            let k = left.min(buf.len() as u64) as usize;
            r.read_exact(&mut buf[..k])?;
            h.update(&buf[..k]);
            left -= k as u64;
        }
    }
    Ok(true)
}

// ------------------------------------------------------------------ state

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Session {
    pub started: i64,
    pub ended: Option<i64>,
    pub files: u64,
    pub bytes: u64,
    pub problems: u64,
    /// Started from the UI (outside the window).
    pub manual: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct WorkerStatus {
    /// off, idle, offline, checking, hashing (fresh files), verifying (a session)
    pub phase: String,
    pub current: Option<String>,
    pub session: Option<Session>,
    pub last_alert: Option<i64>,
    pub alert_error: Option<String>,
}

#[derive(Default)]
struct Ctl {
    run: bool,
    all: bool,
    stop: bool,
    recheck: Vec<i64>,
}

pub struct Integrity {
    ctl: Mutex<Ctl>,
    cv: Condvar,
    pub status: RwLock<WorkerStatus>,
    last_alert_try: Mutex<Option<Instant>>,
}

impl Default for Integrity {
    fn default() -> Self {
        Integrity { ctl: Mutex::new(Ctl::default()), cv: Condvar::new(), status: RwLock::new(WorkerStatus { phase: "off".into(), ..Default::default() }), last_alert_try: Mutex::new(None) }
    }
}

impl Integrity {
    fn wake(&self) {
        self.cv.notify_all();
    }
    fn stop_requested(&self) -> bool {
        self.ctl.lock().stop
    }
    fn set_phase(&self, phase: &str, current: Option<String>) {
        let mut s = self.status.write();
        s.phase = phase.into();
        s.current = current;
    }
    fn set_current(&self, folder: &str, name: &str) {
        self.status.write().current = Some(if folder.is_empty() { name.to_string() } else { format!("{folder}/{name}") });
    }
}

/// Problem counts: (changed, missing, unreadable).
pub fn counts(c: &Connection) -> Result<(i64, i64, i64)> {
    let mut st = c.prepare_cached("SELECT status, count(*) FROM originals WHERE status != 0 GROUP BY status")?;
    let mut out = (0, 0, 0);
    for r in st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))? {
        match r? {
            (CHANGED, n) => out.0 = n,
            (MISSING, n) => out.1 = n,
            (UNREADABLE, n) => out.2 = n,
            _ => {}
        }
    }
    Ok(out)
}

fn emit_counts(state: &AppState) {
    if let Ok(c) = state.db.get()
        && let Ok((changed, missing, unreadable)) = counts(&c)
    {
        state.emit(Event::Integrity { changed, missing, unreadable });
    }
}

// ------------------------------------------------------------------ the index

/// An originals row.
#[derive(Debug, Clone)]
struct Orig {
    id: i64,
    folder: String,
    name: String,
    size: i64,
    scope: i64,
    hash: String,
    status: i64,
}

const ORIG_COLS: &str = "o.id, o.folder, o.name, o.size, o.scope, o.hash, o.status";

fn orig_from(r: &rusqlite::Row) -> rusqlite::Result<Orig> {
    Ok(Orig { id: r.get(0)?, folder: r.get(1)?, name: r.get(2)?, size: r.get(3)?, scope: r.get(4)?, hash: r.get(5)?, status: r.get(6)? })
}

/// A file in the index without a hash yet.
#[derive(Debug, Clone)]
struct Cand {
    folder: String,
    name: String,
    size: i64,
}

fn path_of(state: &AppState, folder: &str, name: &str) -> PathBuf {
    crate::scan::photo_path(state, folder, name)
}

fn file_stamp(p: &Path) -> Option<(i64, i64)> {
    let md = std::fs::metadata(p).ok()?;
    let mtime = md.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis() as i64;
    Some((md.len() as i64, mtime))
}

fn query<T>(c: &Connection, sql: &str, p: impl rusqlite::Params, f: impl FnMut(&rusqlite::Row) -> rusqlite::Result<T>) -> Result<Vec<T>> {
    let mut st = c.prepare(sql)?;
    let rows = st.query_map(p, f)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// What happened to a file.
#[derive(Debug, PartialEq)]
enum Outcome {
    Same,
    /// It was a problem and is not any more.
    Resolved,
    Problem,
    Stopped,
}

/// Record a problem (a new one, or one whose details changed, is alerted again).
fn set_problem(c: &Connection, id: i64, status: i64, detail: &str, new_hash: Option<&str>) -> Result<()> {
    let now = crate::db::now_ms();
    c.execute(
        "UPDATE originals SET
           notified = CASE WHEN status = ?2 AND new_hash IS ?4 THEN notified ELSE 0 END,
           status_at = CASE WHEN status = ?2 AND new_hash IS ?4 THEN status_at ELSE ?5 END,
           detail = CASE WHEN status = ?2 AND new_hash IS ?4 THEN detail ELSE ?3 END,
           status = ?2, new_hash = ?4
         WHERE id = ?1",
        params![id, status, detail, new_hash, now],
    )?;
    Ok(())
}

/// Hash a row's file again and compare.
fn verify(state: &AppState, pace: &mut Pace, o: &Orig) -> Result<(Outcome, u64)> {
    let path = path_of(state, &o.folder, &o.name);
    state.integrity.set_current(&o.folder, &o.name);
    let stamp = file_stamp(&path);
    let res = hash_file(&path, pace);
    let c = state.db.get()?;
    let now = crate::db::now_ms();
    // Size and date as last seen: the next check looks again only if they change.
    let (size, mtime) = stamp.unwrap_or((o.size, 0));
    match res {
        Ok((scope, hash)) => {
            if (scope == o.scope && hash == o.hash) || o.hash.is_empty() {
                // Unchanged, or readable at last (a file that could not be read when first seen).
                c.execute(
                    "UPDATE originals SET size = ?2, mtime = ?3, verified_at = ?4, scope = ?5, hash = ?6, status = 0, status_at = NULL, detail = NULL, new_hash = NULL, notified = 0 WHERE id = ?1",
                    params![o.id, size, mtime, now, scope, hash],
                )?;
                Ok((if o.status == OK { Outcome::Same } else { Outcome::Resolved }, size.max(0) as u64))
            } else {
                let detail = if scope != o.scope { "Its image data can no longer be read as it was" } else { "Its contents differ from when it was first checked" };
                c.execute("UPDATE originals SET size = ?2, mtime = ?3, verified_at = ?4 WHERE id = ?1", params![o.id, size, mtime, now])?;
                set_problem(&c, o.id, CHANGED, detail, Some(&format!("{scope}:{hash}")))?;
                Ok((Outcome::Problem, size.max(0) as u64))
            }
        }
        Err(e) if e.kind() == io::ErrorKind::Interrupted => Ok((Outcome::Stopped, 0)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            // Gone since the last scan: the next index check decides (moved or lost).
            Ok((Outcome::Stopped, 0))
        }
        Err(e) => {
            c.execute("UPDATE originals SET size = ?2, mtime = ?3, verified_at = ?4 WHERE id = ?1", params![o.id, size, mtime, now])?;
            set_problem(&c, o.id, UNREADABLE, &format!("It cannot be read: {e}"), None)?;
            Ok((Outcome::Problem, 0))
        }
    }
}

/// The first hash of a file (or, when it matches a missing original, the end of a move).
fn baseline(state: &AppState, pace: &mut Pace, f: &Cand) -> Result<(Outcome, u64)> {
    let path = path_of(state, &f.folder, &f.name);
    state.integrity.set_current(&f.folder, &f.name);
    let Some((size, mtime)) = file_stamp(&path) else { return Ok((Outcome::Stopped, 0)) };
    let (scope, hash) = match hash_file(&path, pace) {
        Ok(v) => v,
        Err(e) if e.kind() == io::ErrorKind::Interrupted || e.kind() == io::ErrorKind::NotFound => return Ok((Outcome::Stopped, 0)),
        Err(e) => {
            // Reported, without a hash (its first good read becomes the reference).
            let c = state.db.get()?;
            let now = crate::db::now_ms();
            c.execute(
                "INSERT INTO originals(folder, name, size, mtime, scope, hash, first_seen, verified_at, status, status_at, detail)
                 VALUES(?1, ?2, ?3, ?4, -1, '', ?5, ?5, 3, ?5, ?6) ON CONFLICT(folder, name) DO NOTHING",
                params![f.folder, f.name, size, mtime, now, format!("It cannot be read: {e}")],
            )?;
            return Ok((Outcome::Problem, 0));
        }
    };
    record(state, f, size, mtime, scope, &hash)?;
    Ok((Outcome::Same, size as u64))
}

/// An original found at a new place: its record follows (and is fine again).
fn move_row(c: &Connection, id: i64, to: &Cand, size: i64, mtime: i64) -> Result<()> {
    c.execute("DELETE FROM originals WHERE folder = ?1 AND name = ?2 AND id != ?3", params![to.folder, to.name, id])?;
    c.execute(
        "UPDATE originals SET folder = ?2, name = ?3, size = ?4, mtime = ?5, verified_at = ?6, status = 0, status_at = NULL, detail = NULL, new_hash = NULL, notified = 0 WHERE id = ?1",
        params![id, to.folder, to.name, size, mtime, crate::db::now_ms()],
    )?;
    Ok(())
}

/// Record the first hash of a file, unless it is a missing original that moved here.
/// True for a move.
fn record(state: &AppState, f: &Cand, size: i64, mtime: i64, scope: i64, hash: &str) -> Result<bool> {
    let c = state.db.get()?;
    let now = crate::db::now_ms();
    let moved: Option<i64> = c
        .query_row("SELECT id FROM originals WHERE status = 2 AND hash = ?1 AND scope = ?2 AND hash != '' ORDER BY status_at LIMIT 1", params![hash, scope], |r| r.get(0))
        .optional()?;
    if let Some(id) = moved {
        move_row(&c, id, f, size, mtime)?;
        return Ok(true);
    }
    c.execute(
        "INSERT INTO originals(folder, name, size, mtime, scope, hash, first_seen, verified_at) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
         ON CONFLICT(folder, name) DO NOTHING",
        params![f.folder, f.name, size, mtime, scope, hash, now],
    )?;
    Ok(false)
}

/// Reads allowed outside the window: a bucket refilled at `HOURLY_BYTES` an hour.
pub struct Budget {
    tokens: f64,
    at: Instant,
    unlimited: bool,
}

impl Budget {
    pub fn hourly() -> Budget {
        Budget { tokens: HOURLY_BYTES as f64, at: Instant::now(), unlimited: false }
    }
    pub fn unlimited() -> Budget {
        Budget { tokens: 0.0, at: Instant::now(), unlimited: true }
    }
    fn allows(&mut self, bytes: i64) -> bool {
        if self.unlimited {
            return true;
        }
        let now = Instant::now();
        self.tokens = (self.tokens + now.duration_since(self.at).as_secs_f64() * HOURLY_BYTES as f64 / 3600.0).min(HOURLY_BYTES as f64);
        self.at = now;
        // A file bigger than the whole bucket goes once the bucket is full.
        let need = (bytes.max(0) as f64).min(HOURLY_BYTES as f64);
        if self.tokens >= need {
            self.tokens -= need;
            true
        } else {
            false
        }
    }
}

/// When the integrity monitor first ran: files indexed before it (the library as it
/// was) are hashed in the nightly window, not as fresh imports.
fn since(c: &Connection) -> Result<i64> {
    if let Some(v) = crate::db::get_meta(c, "integrity_since") {
        return Ok(v.parse().unwrap_or(0));
    }
    let now = crate::db::now_ms();
    crate::db::set_meta(c, "integrity_since", &now.to_string())?;
    Ok(now)
}

/// What an index check did.
#[derive(Debug, Default)]
pub struct Checked {
    /// Everything was looked at (the budget did not run out, no stop came).
    pub complete: bool,
    pub files: u64,
    pub bytes: u64,
    pub problems: u64,
}

impl Checked {
    fn add(&mut self, out: &Outcome, bytes: u64) {
        if *out != Outcome::Stopped {
            self.files += 1;
            self.bytes += bytes;
        }
        if *out == Outcome::Problem {
            self.problems += 1;
        }
    }
}

/// Follow up on what the last scans found: originals that changed size or date,
/// vanished, or came back, and fresh imports.
pub fn check_index(state: &Arc<AppState>, pace: &mut Pace, budget: &mut Budget) -> Result<Checked> {
    let integ = &state.integrity;
    let mut complete = true;
    let mut problems = false;
    let mut done = Checked::default();
    // 1. Changed in the index (size or date), and missing originals that are back.
    let suspects: Vec<Orig> = {
        let c = state.db.get()?;
        query(
            &c,
            &format!(
                "SELECT {ORIG_COLS} FROM originals o JOIN folders f ON f.path = o.folder JOIN photos p ON p.folder_id = f.id AND p.name = o.name
                 WHERE o.status = 2 OR p.size != o.size OR p.mtime != o.mtime"
            ),
            [],
            orig_from,
        )?
    };
    for o in suspects {
        if integ.stop_requested() || !budget.allows(o.size) {
            complete = false;
            break;
        }
        let (out, bytes) = verify(state, pace, &o)?;
        done.add(&out, bytes);
        match out {
            Outcome::Problem | Outcome::Resolved => problems = true,
            Outcome::Stopped => complete = false,
            Outcome::Same => {}
        }
    }
    // 2. Originals no longer in the index: moved (a new file with the same contents) or lost.
    let gone: Vec<Orig> = {
        let c = state.db.get()?;
        query(
            &c,
            &format!(
                "SELECT {ORIG_COLS} FROM originals o WHERE o.status != 2
                 AND NOT EXISTS (SELECT 1 FROM folders f JOIN photos p ON p.folder_id = f.id WHERE f.path = o.folder AND p.name = o.name)"
            ),
            [],
            orig_from,
        )?
    };
    let gone: Vec<Orig> = gone.into_iter().filter(|o| !path_of(state, &o.folder, &o.name).exists()).collect();
    if !gone.is_empty() {
        let sizes: Vec<i64> = gone.iter().map(|o| o.size).collect();
        let mut pool: HashMap<i64, Vec<Cand>> = HashMap::new();
        {
            let c = state.db.get()?;
            for f in query(
                &c,
                "SELECT f.path, p.name, p.size FROM photos p JOIN folders f ON f.id = p.folder_id
                 WHERE p.size IN (SELECT value FROM json_each(?1))
                 AND NOT EXISTS (SELECT 1 FROM originals o WHERE o.folder = f.path AND o.name = p.name)",
                [serde_json::to_string(&sizes)?],
                |r| Ok(Cand { folder: r.get(0)?, name: r.get(1)?, size: r.get(2)? }),
            )? {
                pool.entry(f.size).or_default().push(f);
            }
        }
        for o in &gone {
            let ext = |n: &str| n.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
            let mut moved = false;
            let cands: Vec<Cand> = pool.get(&o.size).map(|v| v.iter().filter(|c| ext(&c.name) == ext(&o.name)).cloned().collect()).unwrap_or_default();
            // Not held back by the budget: a file vanished as one of the same size
            // appeared, most likely a move, and an alert waits on the answer.
            for cand in cands {
                if integ.stop_requested() {
                    complete = false;
                    break;
                }
                let path = path_of(state, &cand.folder, &cand.name);
                let Some((size, mtime)) = file_stamp(&path) else { continue };
                let Ok((scope, hash)) = hash_file(&path, pace) else { continue };
                done.add(&Outcome::Same, size as u64);
                if let Some(v) = pool.get_mut(&o.size) {
                    v.retain(|c| !(c.folder == cand.folder && c.name == cand.name));
                }
                if scope == o.scope && hash == o.hash && !o.hash.is_empty() {
                    move_row(&*state.db.get()?, o.id, &cand, size, mtime)?;
                    moved = true;
                    break;
                }
                // A file of its own (or where another missing original went).
                record(state, &cand, size, mtime, scope, &hash)?;
            }
            if moved {
                problems |= o.status != OK; // a resolved problem changes the counts too
            } else {
                set_problem(&*state.db.get()?, o.id, MISSING, "The file is no longer in the library", None)?;
                problems = true;
                done.problems += 1;
            }
        }
    }
    // 3. Fresh imports: indexed in the last week, after the monitor started.
    let fresh: Vec<Cand> = {
        let c = state.db.get()?;
        let from = (crate::db::now_ms() - FRESH).max(since(&c)? + 3_600_000);
        query(
            &c,
            "SELECT f.path, p.name, p.size FROM photos p JOIN folders f ON f.id = p.folder_id
             WHERE p.added_at >= ?1 AND NOT EXISTS (SELECT 1 FROM originals o WHERE o.folder = f.path AND o.name = p.name)
             ORDER BY p.added_at DESC LIMIT 5000",
            [from],
            |r| Ok(Cand { folder: r.get(0)?, name: r.get(1)?, size: r.get(2)? }),
        )?
    };
    if !fresh.is_empty() {
        integ.set_phase("hashing", None);
    }
    for f in fresh {
        if integ.stop_requested() || !budget.allows(f.size) {
            complete = false;
            break;
        }
        let (out, bytes) = baseline(state, pace, &f)?;
        done.add(&out, bytes);
        match out {
            Outcome::Stopped => complete = false,
            Outcome::Problem => problems = true,
            _ => {}
        }
    }
    if problems {
        emit_counts(state);
    }
    done.complete = complete;
    Ok(done)
}

/// Verify the files the UI asked for.
fn rechecks(state: &Arc<AppState>, pace: &mut Pace, ids: &[i64]) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let rows: Vec<Orig> = {
        let c = state.db.get()?;
        query(&c, &format!("SELECT {ORIG_COLS} FROM originals o WHERE o.id IN (SELECT value FROM json_each(?1))"), [serde_json::to_string(ids)?], orig_from)?
    };
    state.integrity.set_phase("verifying", None);
    for o in rows {
        if path_of(state, &o.folder, &o.name).exists() {
            verify(state, pace, &o)?;
        }
    }
    emit_counts(state);
    Ok(())
}

/// A verification session: what changed since the last scans, the first hash of every
/// file that has none, then the files due again (every file once per `verify_days`,
/// a day's share each night). `all`: every file, however recently verified.
pub fn session(state: &Arc<AppState>, pace: &mut Pace, until: Option<Instant>, manual: bool, all: bool) -> Result<Session> {
    let integ = &state.integrity;
    let started = crate::db::now_ms();
    let mut s = Session { started, manual, ..Default::default() };
    integ.status.write().session = Some(s.clone());
    let over = || integ.stop_requested() || until.is_some_and(|u| Instant::now() >= u);
    integ.set_phase("verifying", None);
    let mut unlimited = Budget::unlimited();
    let first = check_index(state, pace, &mut unlimited)?;
    (s.files, s.bytes, s.problems) = (first.files, first.bytes, first.problems);

    let tally = |s: &mut Session, out: Outcome, bytes: u64| {
        if out != Outcome::Stopped {
            s.files += 1;
            s.bytes += bytes;
        }
        if out == Outcome::Problem {
            s.problems += 1;
        }
        integ.status.write().session = Some(s.clone());
    };
    // Files that could not be read: try again, once a session.
    let unreadable: Vec<Orig> = {
        let c = state.db.get()?;
        query(&c, &format!("SELECT {ORIG_COLS} FROM originals o WHERE o.status = 3 AND o.verified_at < ?1"), [started], orig_from)?
    };
    for o in &unreadable {
        if over() {
            break;
        }
        let (out, bytes) = verify(state, pace, o)?;
        tally(&mut s, out, bytes);
    }
    // The first hash of every file: newest folders first, a folder at a time.
    let mut key: Option<(String, String)> = None;
    'base: loop {
        let batch: Vec<Cand> = {
            let c = state.db.get()?;
            let (kp, kn) = key.clone().unzip();
            query(
                &c,
                "SELECT f.path, p.name, p.size FROM folders f JOIN photos p ON p.folder_id = f.id
                 WHERE (?1 IS NULL OR (f.path, p.name) < (?1, ?2))
                 AND NOT EXISTS (SELECT 1 FROM originals o WHERE o.folder = f.path AND o.name = p.name)
                 ORDER BY f.path DESC, p.name DESC LIMIT 500",
                params![kp, kn],
                |r| Ok(Cand { folder: r.get(0)?, name: r.get(1)?, size: r.get(2)? }),
            )?
        };
        let Some(last) = batch.last() else { break };
        key = Some((last.folder.clone(), last.name.clone()));
        for f in &batch {
            if over() {
                break 'base;
            }
            let (out, bytes) = baseline(state, pace, f)?;
            tally(&mut s, out, bytes);
        }
    }
    // Re-verification: what is due, then (in the night) the day's share.
    let (days, total): (i64, i64) = {
        let c = state.db.get()?;
        (state.cfg.verify_days.max(1) as i64, c.query_row("SELECT COALESCE(sum(size), 0) FROM originals WHERE status = 0", [], |r| r.get(0))?)
    };
    let share = if manual { 0 } else { (total / days) as u64 };
    let due_before = if all { started } else { started - days * 86_400_000 };
    let mut shared = 0u64;
    let mut after = (i64::MIN, 0i64);
    'verify: loop {
        let batch: Vec<(Orig, i64)> = {
            let c = state.db.get()?;
            query(
                &c,
                &format!("SELECT {ORIG_COLS}, o.verified_at FROM originals o WHERE o.status = 0 AND o.verified_at < ?1 AND (o.verified_at, o.id) > (?2, ?3) ORDER BY o.verified_at, o.id LIMIT 500"),
                params![started, after.0, after.1],
                |r| Ok((orig_from(r)?, r.get(7)?)),
            )?
        };
        if batch.is_empty() {
            break;
        }
        for (o, at) in &batch {
            after = (*at, o.id);
            if *at >= due_before && shared >= share {
                break 'verify;
            }
            if over() {
                break 'verify;
            }
            let (out, bytes) = verify(state, pace, o)?;
            if *at >= due_before {
                shared += bytes;
            }
            tally(&mut s, out, bytes);
        }
    }
    s.ended = Some(crate::db::now_ms());
    integ.status.write().session = Some(s.clone());
    // Kept for the next start, so the UI can still say when the last check was.
    if let Ok(c) = state.db.get() {
        let _ = crate::db::set_meta(&c, "integrity_last", &serde_json::to_string(&s).unwrap_or_default());
    }
    emit_counts(state);
    Ok(s)
}

// ------------------------------------------------------------------ the window

/// Minutes since local midnight (the container's TZ).
fn local_minutes() -> i64 {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    #[cfg(unix)]
    unsafe {
        let t = now as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        if !libc::localtime_r(&t, &mut tm).is_null() {
            return tm.tm_hour as i64 * 60 + tm.tm_min as i64;
        }
    }
    (now / 60).rem_euclid(1440)
}

/// (open now, time left in it or until it opens).
fn window(cfg: &crate::config::Config) -> (bool, Duration) {
    let start = (cfg.verify_hour.min(23) * 60) as i64;
    let len = (cfg.verify_minutes.clamp(1, 1440)) as i64;
    let into = (local_minutes() - start).rem_euclid(1440);
    if into < len {
        (true, Duration::from_secs(((len - into) * 60) as u64))
    } else {
        (false, Duration::from_secs(((1440 - into) * 60) as u64))
    }
}

// ------------------------------------------------------------------ alerts

/// How alerts are sent: the format named, or guessed from the address.
fn alert_format(cfg: &crate::config::Config) -> &'static str {
    let url = cfg.alert_url.as_deref().unwrap_or("");
    match cfg.alert_format.to_ascii_lowercase().as_str() {
        "ntfy" => "ntfy",
        "gotify" => "gotify",
        "json" | "webhook" => "json",
        _ => {
            let path = url.split('?').next().unwrap_or("");
            if path.trim_end_matches('/').ends_with("/message") {
                "gotify"
            } else if path.contains("/webhook") || path.contains("/hooks/") {
                "json"
            } else {
                "ntfy"
            }
        }
    }
}

fn send_alert(cfg: &crate::config::Config, title: &str, message: &str, priority: u8, extra: Value) -> Result<(), String> {
    let url = cfg.alert_url.as_deref().ok_or("no alert address is set (HYPERFOCAL_ALERT_URL)")?;
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(20))).http_status_as_error(false).build().into();
    let fmt = alert_format(cfg);
    let res = match fmt {
        "gotify" => {
            let mut req = agent.post(url);
            if let Some(t) = &cfg.alert_token {
                req = req.header("X-Gotify-Key", t);
            }
            req.content_type("application/json").send(json!({ "title": title, "message": message, "priority": if priority >= 4 { 8 } else { 4 } }).to_string())
        }
        "json" => {
            let mut body = json!({ "title": title, "message": message, "source": "hyperfocal" });
            if let (Some(b), Value::Object(e)) = (body.as_object_mut(), extra) {
                b.extend(e);
            }
            let mut req = agent.post(url);
            if let Some(t) = &cfg.alert_token {
                req = req.header("Authorization", &format!("Bearer {t}"));
            }
            req.content_type("application/json").send(body.to_string())
        }
        _ => {
            let mut req = agent.post(url).header("Title", title).header("Priority", &priority.to_string()).header("Tags", if priority >= 4 { "warning" } else { "white_check_mark" });
            if let Some(t) = &cfg.alert_token {
                req = req.header("Authorization", &format!("Bearer {t}"));
            }
            req.content_type("text/plain; charset=utf-8").send(message)
        }
    };
    match res {
        Ok(r) if r.status().is_success() => Ok(()),
        Ok(r) => Err(format!("{} answered {}", host_of(url), r.status())),
        Err(e) => Err(format!("{}: {e}", host_of(url))),
    }
}

fn host_of(url: &str) -> String {
    url.split("://").nth(1).unwrap_or(url).split(['/', '?']).next().unwrap_or("").rsplit('@').next().unwrap_or("").to_string()
}

/// Send one alert for the problems not yet reported (once their grace has passed).
pub fn alerts(state: &AppState, force: bool) -> Result<usize> {
    if state.cfg.alert_url.is_none() {
        return Ok(0);
    }
    {
        let last = state.integrity.last_alert_try.lock();
        if !force && last.is_some_and(|t| t.elapsed() < ALERT_EVERY) {
            return Ok(0);
        }
    }
    let now = crate::db::now_ms();
    let rows: Vec<(i64, String, i64)> = {
        let c = state.db.get()?;
        query(
            &c,
            "SELECT id, CASE WHEN folder = '' THEN name ELSE folder || '/' || name END, status FROM originals
             WHERE status != 0 AND notified = 0 AND status_at <= CASE status WHEN 2 THEN ?1 ELSE ?2 END ORDER BY status_at LIMIT 2000",
            params![now - GRACE_MISSING, now - GRACE_OTHER],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?
    };
    if rows.is_empty() {
        return Ok(0);
    }
    *state.integrity.last_alert_try.lock() = Some(Instant::now());
    let n = |k: i64| rows.iter().filter(|r| r.2 == k).count();
    let (changed, missing, unreadable) = (n(CHANGED), n(MISSING), n(UNREADABLE));
    let mut parts = Vec::new();
    for (k, w) in [(changed, "changed"), (missing, "missing"), (unreadable, "unreadable")] {
        if k > 0 {
            parts.push(format!("{k} {w}"));
        }
    }
    let title = format!("Hyperfocal: {} original{} {}", rows.len(), if rows.len() == 1 { "" } else { "s" }, parts.join(", "));
    let word = |s: i64| match s {
        CHANGED => "Changed",
        MISSING => "Missing",
        _ => "Unreadable",
    };
    let mut lines: Vec<String> = rows.iter().take(15).map(|r| format!("{}: {}", word(r.2), r.1)).collect();
    if rows.len() > 15 {
        lines.push(format!("…and {} more", rows.len() - 15));
    }
    lines.push("Review them in Hyperfocal: Tools › Archive integrity.".into());
    let list = |k: i64| rows.iter().filter(|r| r.2 == k).map(|r| r.1.clone()).collect::<Vec<_>>();
    let extra = json!({ "changed": list(CHANGED), "missing": list(MISSING), "unreadable": list(UNREADABLE) });
    match send_alert(&state.cfg, &title, &lines.join("\n"), 4, extra) {
        Ok(()) => {
            let ids: Vec<i64> = rows.iter().map(|r| r.0).collect();
            let c = state.db.get()?;
            c.execute("UPDATE originals SET notified = 1 WHERE id IN (SELECT value FROM json_each(?1))", [serde_json::to_string(&ids)?])?;
            let mut s = state.integrity.status.write();
            s.last_alert = Some(now);
            s.alert_error = None;
            Ok(rows.len())
        }
        Err(e) => {
            tracing::warn!("integrity alert not sent: {e}");
            state.integrity.status.write().alert_error = Some(e);
            Ok(0)
        }
    }
}

// ------------------------------------------------------------------ the worker

pub fn spawn(state: Arc<AppState>) {
    if !state.cfg.integrity {
        return;
    }
    state.integrity.set_phase("idle", None);
    if let Ok(c) = state.db.get() {
        state.integrity.status.write().session = crate::db::get_meta(&c, "integrity_last").and_then(|v| serde_json::from_str(&v).ok());
    }
    std::thread::Builder::new()
        .name("integrity".into())
        .spawn(move || {
            // Give the first scan a head start.
            std::thread::sleep(Duration::from_secs(20));
            worker(&state)
        })
        .expect("spawn integrity");
}

fn worker(state: &Arc<AppState>) {
    use tokio::sync::broadcast::error::TryRecvError;
    let mut rx = state.events.subscribe();
    let integ = &state.integrity;
    let mut dirty = true;
    let mut quiet = Instant::now() - Duration::from_secs(3600);
    let mut last_check = Instant::now() - Duration::from_secs(7200);
    let mut budget = Budget::hourly();
    let busy = || state.images.busy();
    let stop = || integ.stop_requested();
    loop {
        loop {
            match rx.try_recv() {
                Ok(Event::Folder { .. } | Event::Tree) | Err(TryRecvError::Lagged(_)) => {
                    dirty = true;
                    quiet = Instant::now();
                }
                Ok(_) => {}
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Closed) => return,
            }
        }
        let (library_ok, scanning) = {
            let p = state.progress.read();
            (p.library_ok, !matches!(p.phase.as_str(), "idle"))
        };
        let (recheck, run, all) = {
            let mut c = integ.ctl.lock();
            c.stop = false;
            (std::mem::take(&mut c.recheck), std::mem::take(&mut c.run), std::mem::take(&mut c.all))
        };
        if !library_ok {
            integ.set_phase("offline", None);
        } else {
            let mut pace = Pace::new(state.cfg.hash_mbps, &busy, &stop);
            let r: Result<()> = (|| {
                rechecks(state, &mut pace, &recheck)?;
                let (open, left) = window(&state.cfg);
                if run || open {
                    let until = if run { None } else { Some(Instant::now() + left) };
                    let s = session(state, &mut pace, until, run, all)?;
                    tracing::info!("integrity: checked {} files ({} MB), {} problems", s.files, s.bytes >> 20, s.problems);
                    last_check = Instant::now();
                    dirty = false;
                    let (still, rest) = window(&state.cfg);
                    if open && still && !run {
                        // Done for tonight while the window is still open.
                        integ.set_phase("idle", None);
                        wait(state, rest);
                    }
                } else if (dirty && quiet.elapsed() > Duration::from_secs(30) && !scanning) || last_check.elapsed() > Duration::from_secs(3600) {
                    integ.set_phase("checking", None);
                    check_index(state, &mut pace, &mut budget)?;
                    last_check = Instant::now();
                    dirty = false;
                }
                alerts(state, false)?;
                Ok(())
            })();
            if let Err(e) = r {
                tracing::warn!("integrity: {e:#}");
            }
            integ.set_phase("idle", None);
        }
        let (_, left) = window(&state.cfg);
        wait(state, left.min(Duration::from_secs(30)));
    }
}

/// Sleep until `d` passes or the UI asks for something.
fn wait(state: &AppState, d: Duration) {
    let mut c = state.integrity.ctl.lock();
    if c.run || !c.recheck.is_empty() {
        return;
    }
    state.integrity.cv.wait_for(&mut c, d);
}

// ------------------------------------------------------------------ API

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/integrity", get(summary))
        .route("/integrity/recheck", post(recheck))
        .route("/integrity/accept", post(accept))
        .route("/integrity/forget", post(forget))
        .route("/integrity/run", post(run))
        .route("/integrity/test-alert", post(test_alert))
}

fn enabled(state: &AppState) -> ApiResult<()> {
    if !state.cfg.integrity {
        return Err(ApiError(StatusCode::CONFLICT, "the integrity monitor is off (HYPERFOCAL_INTEGRITY=false)".into()));
    }
    Ok(())
}

async fn summary(State(state): St) -> ApiResult<Json<Value>> {
    let st = state.clone();
    let v = tokio::task::spawn_blocking(move || -> Result<Value> {
        let c = st.db.get()?;
        let now = crate::db::now_ms();
        let days = st.cfg.verify_days.max(1) as i64;
        let (originals, bytes, recent, oldest): (i64, i64, i64, Option<i64>) = c.query_row(
            "SELECT count(*), COALESCE(sum(size), 0), COALESCE(sum(verified_at >= ?1), 0), min(verified_at) FROM originals",
            [now - days * 86_400_000],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        let indexed: i64 = c.query_row("SELECT count(*) FROM photos", [], |r| r.get(0))?;
        let (changed, missing, unreadable) = counts(&c)?;
        let problems = query(
            &c,
            "SELECT o.id, o.folder, o.name, o.status, o.status_at, o.detail, o.size, o.verified_at, o.first_seen, o.notified, f.id, COALESCE(p.pair_of, p.id)
             FROM originals o LEFT JOIN folders f ON f.path = o.folder LEFT JOIN photos p ON p.folder_id = f.id AND p.name = o.name
             WHERE o.status != 0 ORDER BY o.status_at DESC, o.id LIMIT 2000",
            [],
            |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?, "folder": r.get::<_, String>(1)?, "name": r.get::<_, String>(2)?,
                    "status": r.get::<_, i64>(3)?, "since": r.get::<_, Option<i64>>(4)?, "detail": r.get::<_, Option<String>>(5)?,
                    "size": r.get::<_, i64>(6)?, "verified_at": r.get::<_, i64>(7)?, "first_seen": r.get::<_, i64>(8)?,
                    "notified": r.get::<_, i64>(9)? != 0, "folder_id": r.get::<_, Option<i64>>(10)?, "photo_id": r.get::<_, Option<i64>>(11)?,
                }))
            },
        )?;
        let (open, left) = window(&st.cfg);
        let next = if open { now } else { now + left.as_millis() as i64 };
        let ws = st.integrity.status.read().clone();
        Ok(json!({
            "enabled": st.cfg.integrity,
            "originals": originals, "indexed": indexed, "bytes": bytes,
            "verified_recently": recent, "oldest_verified": oldest,
            "changed": changed, "missing": missing, "unreadable": unreadable,
            "problems": problems,
            "worker": ws,
            "window": { "open": open, "next": next, "hour": st.cfg.verify_hour, "minutes": st.cfg.verify_minutes },
            "days": days, "mbps": st.cfg.hash_mbps,
            "alert": st.cfg.alert_url.as_deref().map(|u| json!({ "host": host_of(u), "format": alert_format(&st.cfg) })),
        }))
    })
    .await
    .map_err(|e| anyhow::anyhow!("{e}"))??;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct Ids {
    ids: Vec<i64>,
}

fn check_ids(b: &Ids) -> ApiResult<()> {
    if b.ids.is_empty() || b.ids.len() > 100_000 {
        return Err(bad("between 1 and 100000 files"));
    }
    Ok(())
}

async fn recheck(State(state): St, Json(b): Json<Ids>) -> ApiResult<Json<Value>> {
    enabled(&state)?;
    check_ids(&b)?;
    let mut c = state.integrity.ctl.lock();
    c.recheck.extend(b.ids);
    drop(c);
    state.integrity.wake();
    Ok(Json(json!({ "ok": true })))
}

/// Take the current contents of changed files as the new reference.
async fn accept(State(state): St, Json(b): Json<Ids>) -> ApiResult<Json<Value>> {
    check_ids(&b)?;
    let st = state.clone();
    let n = tokio::task::spawn_blocking(move || -> Result<usize> {
        let mut c = st.db.get()?;
        let tx = c.transaction()?;
        let rows: Vec<(i64, String)> = query(&tx, "SELECT id, new_hash FROM originals WHERE status = 1 AND new_hash IS NOT NULL AND id IN (SELECT value FROM json_each(?1))", [serde_json::to_string(&b.ids)?], |r| Ok((r.get(0)?, r.get(1)?)))?;
        let now = crate::db::now_ms();
        for (id, nh) in &rows {
            let Some((scope, hash)) = nh.split_once(':') else { continue };
            tx.execute(
                "UPDATE originals SET scope = ?2, hash = ?3, status = 0, status_at = NULL, detail = NULL, new_hash = NULL, notified = 0, verified_at = ?4 WHERE id = ?1",
                params![id, scope.parse::<i64>().unwrap_or(0), hash, now],
            )?;
        }
        tx.commit()?;
        Ok(rows.len())
    })
    .await
    .map_err(|e| anyhow::anyhow!("{e}"))??;
    emit_counts(&state);
    Ok(Json(json!({ "accepted": n })))
}

/// Stop reporting missing files (they were deleted on purpose).
async fn forget(State(state): St, Json(b): Json<Ids>) -> ApiResult<Json<Value>> {
    check_ids(&b)?;
    let st = state.clone();
    let n = tokio::task::spawn_blocking(move || -> Result<usize> {
        let c = st.db.get()?;
        Ok(c.execute("DELETE FROM originals WHERE status = 2 AND id IN (SELECT value FROM json_each(?1))", [serde_json::to_string(&b.ids)?])?)
    })
    .await
    .map_err(|e| anyhow::anyhow!("{e}"))??;
    emit_counts(&state);
    Ok(Json(json!({ "forgotten": n })))
}

#[derive(Deserialize)]
struct RunBody {
    #[serde(default)]
    all: bool,
    #[serde(default)]
    stop: bool,
}

async fn run(State(state): St, Json(b): Json<RunBody>) -> ApiResult<Json<Value>> {
    enabled(&state)?;
    let mut c = state.integrity.ctl.lock();
    if b.stop {
        c.stop = true;
        c.run = false;
        c.all = false;
    } else {
        c.run = true;
        c.all |= b.all;
    }
    drop(c);
    state.integrity.wake();
    Ok(Json(json!({ "ok": true })))
}

async fn test_alert(State(state): St) -> ApiResult<Json<Value>> {
    let st = state.clone();
    tokio::task::spawn_blocking(move || send_alert(&st.cfg, "Hyperfocal test alert", "Alerts from Hyperfocal's archive integrity monitor reach you.", 3, json!({ "test": true })))
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .map_err(|e| ApiError(StatusCode::BAD_GATEWAY, e))?;
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash_bytes(name: &str, data: &[u8]) -> (i64, String) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(name);
        std::fs::write(&p, data).unwrap();
        hash_file(&p, &mut Pace::unlimited()).unwrap()
    }

    /// A small baseline JPEG: SOI, APP1 (EXIF-like), DQT, SOF0, DHT, SOS, data, EOI.
    fn jpeg(app: &[u8], data: &[u8]) -> Vec<u8> {
        let seg = |m: u8, body: &[u8]| [&[0xFF, m][..], &((body.len() + 2) as u16).to_be_bytes(), body].concat();
        [
            vec![0xFF, 0xD8],
            seg(0xE1, app),
            seg(0xDB, &[0u8; 65]),
            seg(0xC0, &[8, 0, 16, 0, 16, 1, 1, 0x11, 0]),
            seg(0xFE, b"a comment"),
            seg(0xC4, &[0u8; 20]),
            seg(0xDA, &[1, 1, 0, 0, 63, 0]),
            data.to_vec(),
            vec![0xFF, 0xD9],
        ]
        .concat()
    }

    #[test]
    fn jpeg_metadata_does_not_count() {
        let a = hash_bytes("a.jpg", &jpeg(b"Exif\0\0rating=1", &[1, 2, 0xFF, 0x00, 3, 0xFF, 0xD3, 4]));
        assert_eq!(a.0, SCOPE_JPEG);
        // Other metadata (and trailing bytes after the image) leave the hash alone.
        let mut b = jpeg(b"Exif\0\0rating=5 and a longer caption", &[1, 2, 0xFF, 0x00, 3, 0xFF, 0xD3, 4]);
        b.extend_from_slice(b"trailing video");
        assert_eq!(hash_bytes("b.jpg", &b), a);
        // One bit of the image data does not.
        assert_ne!(hash_bytes("c.jpg", &jpeg(b"Exif\0\0rating=1", &[1, 2, 0xFF, 0x00, 3, 0xFF, 0xD3, 5])), a);
        // Broken structure: the whole file, under its own scope.
        let mut cut = jpeg(b"x", &[1, 2, 3]);
        cut.truncate(cut.len() - 2);
        assert_eq!(hash_bytes("d.jpg", &cut).0, SCOPE_FILE);
        assert_eq!(hash_bytes("e.cr3", b"raw bytes").0, SCOPE_FILE);
        assert_ne!(hash_bytes("f.cr3", b"raw bytes"), hash_bytes("g.cr3", b"raw byteZ"));
    }

    /// A little-endian TIFF: IFD0 is a 2x1 preview (marked reduced-resolution) whose
    /// SubIFD is the 4x2 main image in two strips; metadata comes after all of it.
    fn tiff(main: &[u8; 8], preview: &[u8; 2], meta: &[u8]) -> Vec<u8> {
        let ent = |tag: u16, typ: u16, count: u32, val: u32| [tag.to_le_bytes().as_slice(), &typ.to_le_bytes(), &count.to_le_bytes(), &val.to_le_bytes()].concat();
        let mut f = b"II*\0".to_vec();
        f.extend_from_slice(&8u32.to_le_bytes());
        // IFD0 at 8: 2 + 6 * 12 + 4 bytes.
        f.extend_from_slice(&6u16.to_le_bytes());
        for e in [ent(254, 4, 1, 1), ent(256, 3, 1, 2), ent(257, 3, 1, 1), ent(273, 4, 1, 148), ent(279, 4, 1, 2), ent(330, 4, 1, 86)] {
            f.extend(e);
        }
        f.extend_from_slice(&0u32.to_le_bytes());
        assert_eq!(f.len(), 86);
        // The SubIFD at 86: 2 + 4 * 12 + 4 bytes; strip offsets at 140, byte counts (4, 4) inline.
        f.extend_from_slice(&4u16.to_le_bytes());
        for e in [ent(256, 3, 1, 4), ent(257, 3, 1, 2), ent(273, 4, 2, 140), ent(279, 3, 2, 0x0004_0004)] {
            f.extend(e);
        }
        f.extend_from_slice(&0u32.to_le_bytes());
        assert_eq!(f.len(), 140);
        f.extend_from_slice(&150u32.to_le_bytes());
        f.extend_from_slice(&154u32.to_le_bytes());
        f.extend_from_slice(preview); // 148..150
        f.extend_from_slice(main); // 150..158
        f.extend_from_slice(meta);
        f
    }

    #[test]
    fn tiff_main_image_only() {
        let a = hash_bytes("a.dng", &tiff(b"12345678", b"pp", b"<xmp rating=1/>"));
        assert_eq!(a.0, SCOPE_TIFF);
        assert_eq!(hash_bytes("b.dng", &tiff(b"12345678", b"qq", b"<xmp rating=5 longer/>")), a, "preview and metadata do not count");
        assert_ne!(hash_bytes("c.dng", &tiff(b"12345679", b"pp", b"<xmp rating=1/>")), a);
        assert_eq!(hash_bytes("d.tif", b"II*\0garbage").0, SCOPE_FILE);
    }

    #[test]
    fn budget_and_window() {
        let mut b = Budget::hourly();
        assert!(b.allows(3 << 30));
        assert!(!b.allows(2 << 30), "the bucket holds 4 GB");
        assert!(Budget::unlimited().allows(i64::MAX));
        let mut cfg = crate::config::Config::for_tests(Path::new("/tmp"));
        cfg.verify_minutes = 1440;
        assert!(window(&cfg).0, "a 24-hour window is always open");
        cfg.verify_minutes = 60;
        let (open, left) = window(&cfg);
        assert!(left <= Duration::from_secs(if open { 3600 } else { 86_400 }));
    }

    #[test]
    fn alert_formats() {
        let mut cfg = crate::config::Config::for_tests(Path::new("/tmp"));
        for (url, want) in [
            ("https://ntfy.sh/my-photos", "ntfy"),
            ("https://push.example.com/message?token=abc", "gotify"),
            ("http://ha.local:8123/api/webhook/hyperfocal", "json"),
        ] {
            cfg.alert_url = Some(url.into());
            assert_eq!(alert_format(&cfg), want, "{url}");
        }
        cfg.alert_format = "json".into();
        assert_eq!(alert_format(&cfg), "json");
        assert_eq!(host_of("https://user:pw@ntfy.example.com/topic?x=1"), "ntfy.example.com");
    }
}
