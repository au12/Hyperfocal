//! The background worker: a perceptual hash (and a focus estimate) of every photo,
//! from its thumbnail.
//!
//! Passes go from the newest photo down, so a fresh import is covered first; a folder the
//! Duplicates report was asked about goes ahead of everything. Thumbnails the thumbnail
//! builder is about to make are waited for rather than made twice; any other missing
//! thumbnail is made in memory (and not kept). The worker steps aside whenever someone is
//! waiting on an image and while the library is being scanned.

use super::phash;
use crate::model::PhotoRec;
use crate::repo;
use crate::state::{AppState, Event};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A photo that could not be read is tried again after this long (or once its file changes).
const RETRY_MS: i64 = 7 * 86_400_000;
const BATCH: i64 = 64;
/// A pass also starts this often without a change on disk (rotations, missed events).
const EVERY: Duration = Duration::from_secs(1800);
/// Passes for changes on disk start at most this often.
const MIN_GAP: Duration = Duration::from_secs(60);

/// The photos that need a (new) hash: shown ones outside the reject folders, never
/// hashed, hashed from another state of the file, or unreadable a while ago.
const NEEDS: &str = "p.pair_of IS NULL AND f.is_reject = 0
     AND (h.photo_id IS NULL OR h.size != p.size OR h.mtime != p.mtime OR h.orientation != p.orientation
          OR (h.hash IS NULL AND h.at < ?1))";

pub fn spawn(state: Arc<AppState>) {
    if !state.cfg.dupes {
        return;
    }
    set_phase(&state, "idle", None);
    std::thread::Builder::new()
        .name("dupes".into())
        .spawn(move || {
            // Give the first scan and the thumbnails a head start.
            std::thread::sleep(Duration::from_secs(15));
            worker(&state)
        })
        .expect("spawn dupes");
}

fn set_phase(state: &AppState, phase: &str, current: Option<String>) {
    let mut s = state.dupes.status.write();
    s.phase = phase.into();
    s.current = current;
}

/// How many photos need a hash.
pub fn pending(c: &Connection) -> Result<i64> {
    let sql = format!("SELECT count(*) FROM photos p JOIN folders f ON f.id = p.folder_id LEFT JOIN phashes h ON h.photo_id = p.id WHERE {NEEDS}");
    Ok(c.query_row(&sql, [crate::db::now_ms() - RETRY_MS], |r| r.get(0))?)
}

/// The next photos of this pass that need a hash, below `cursor` (newest first), as
/// much of each as names its thumbnail.
fn next_batch(c: &Connection, cursor: i64) -> Result<Vec<PhotoRec>> {
    let sql = format!(
        "SELECT p.id, p.size, p.mtime, p.orientation FROM photos p JOIN folders f ON f.id = p.folder_id
         LEFT JOIN phashes h ON h.photo_id = p.id WHERE p.id < ?2 AND {NEEDS} ORDER BY p.id DESC LIMIT {BATCH}"
    );
    let mut st = c.prepare_cached(&sql)?;
    let rows = st.query_map(params![crate::db::now_ms() - RETRY_MS, cursor], |r| {
        Ok(PhotoRec { id: r.get(0)?, size: r.get(1)?, mtime: r.get(2)?, orientation: r.get(3)?, ..Default::default() })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// The same for one photo (a priority one), if it needs a hash.
fn needs(c: &Connection, id: i64) -> Result<Option<PhotoRec>> {
    let sql = format!(
        "SELECT p.id, p.size, p.mtime, p.orientation FROM photos p JOIN folders f ON f.id = p.folder_id
         LEFT JOIN phashes h ON h.photo_id = p.id WHERE p.id = ?2 AND {NEEDS}"
    );
    let r = c
        .prepare_cached(&sql)?
        .query_row(params![crate::db::now_ms() - RETRY_MS, id], |r| {
            Ok(PhotoRec { id: r.get(0)?, size: r.get(1)?, mtime: r.get(2)?, orientation: r.get(3)?, ..Default::default() })
        })
        .optional()?;
    Ok(r)
}

enum Outcome {
    Done,
    /// Its thumbnail is queued: hash it once that is built.
    Later,
    Gone,
}

/// Hash one photo and store the result (or why it could not be read).
fn hash_one(state: &AppState, row: &PhotoRec) -> Result<Outcome> {
    // Before anything else: a thumbnail on its way is waited for.
    if state.cfg.pregen_thumbs && state.images.thumbs_queued() > 0 && !state.images.has_thumb(row) {
        return Ok(Outcome::Later);
    }
    let loc = {
        let c = state.db.get()?;
        match repo::load(&c, row.id)? {
            Some(l) if !l.folder_is_reject => l,
            _ => return Ok(Outcome::Gone),
        }
    };
    let id = loc.rec.id;
    let label = if loc.folder_path.is_empty() { loc.rec.name.clone() } else { format!("{}/{}", loc.folder_path, loc.rec.name) };
    set_phase(state, "hashing", Some(label));
    let (hash, sharp, error) = match crate::images::thumb_pixels(state, &loc) {
        Ok(img) => (Some(phash::phash(&img) as i64), Some(phash::sharpness(&img) as f64), None),
        Err(e) => (None, None, Some(format!("{e:#}"))),
    };
    let c = state.db.get()?;
    c.execute(
        "INSERT INTO phashes(photo_id, size, mtime, orientation, hash, sharp, at, error) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(photo_id) DO UPDATE SET size = excluded.size, mtime = excluded.mtime, orientation = excluded.orientation,
           hash = excluded.hash, sharp = excluded.sharp, at = excluded.at, error = excluded.error",
        params![id, loc.rec.size, loc.rec.mtime, loc.rec.orientation, hash, sharp, crate::db::now_ms(), error],
    )
    .map(|_| Outcome::Done)
    // The photo was deleted meanwhile (foreign key).
    .or(Ok(Outcome::Gone))
}

/// Hash everything that needs it, now, without stepping aside for anyone (tests).
pub fn hash_now(state: &AppState) -> Result<usize> {
    let phase = state.dupes.status.read().phase.clone();
    let mut n = 0;
    let mut cursor = i64::MAX;
    loop {
        let batch = next_batch(&*state.db.get()?, cursor)?;
        let Some(last) = batch.last() else { break };
        cursor = last.id;
        for row in &batch {
            if let Outcome::Done = hash_one(state, row)? {
                n += 1;
            }
        }
    }
    set_phase(state, &phase, None);
    Ok(n)
}

/// Wait while someone is looking at photos or the library is being scanned.
fn yield_to_others(state: &AppState) {
    let mut waited = false;
    loop {
        let scanning = state.progress.read().phase != "idle";
        if !state.images.busy() && !scanning {
            break;
        }
        if scanning && !waited {
            set_phase(state, "waiting", None);
            waited = true;
        }
        std::thread::sleep(Duration::from_millis(if scanning { 1000 } else { 50 }));
    }
}

fn worker(state: &Arc<AppState>) {
    use tokio::sync::broadcast::error::TryRecvError;
    let mut rx = state.events.subscribe();
    let dupes = &state.dupes;
    // A pass is due at the start, after a change on disk (at most one a minute: rejects
    // while culling come quickly), and now and then regardless.
    let mut due = true;
    let mut last_pass: Option<Instant> = None;
    loop {
        loop {
            match rx.try_recv() {
                Ok(Event::Folder { .. } | Event::Tree) | Err(TryRecvError::Lagged(_)) => due = true,
                Ok(_) => {}
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Closed) => return,
            }
        }
        let priority = !dupes.ctl.lock().priority.is_empty();
        let since = last_pass.map_or(Duration::MAX, |t| t.elapsed());
        let mut wait = Duration::from_secs(30);
        if priority || (due && since >= MIN_GAP) || since >= EVERY {
            due = false;
            last_pass = Some(Instant::now());
            match pass(state) {
                Ok((done, later)) => {
                    if later > 0 {
                        // Thumbnails were still being built: back for those photos in a while.
                        set_phase(state, "waiting", None);
                        due = true;
                        wait = Duration::from_secs(120);
                    } else {
                        set_phase(state, "idle", None);
                        dupes.status.write().pending = 0;
                    }
                    if done > 0 {
                        state.emit(Event::Dupes);
                    }
                }
                Err(e) => {
                    tracing::warn!("duplicates: {e:#}");
                    set_phase(state, "idle", None);
                }
            }
        } else if due {
            wait = MIN_GAP.saturating_sub(since).max(Duration::from_secs(1));
        }
        let mut c = dupes.ctl.lock();
        if !c.wake && c.priority.is_empty() {
            dupes.cv.wait_for(&mut c, wait);
        }
        c.wake = false;
    }
}

/// One pass over everything that needs a hash, priority photos first. Returns how many
/// photos it hashed, and how many wait for their thumbnails.
fn pass(state: &AppState) -> Result<(usize, usize)> {
    let dupes = &state.dupes;
    let todo = pending(&*state.db.get()?)?;
    {
        let mut s = dupes.status.write();
        s.pending = todo;
        s.done = 0;
    }
    if todo == 0 && dupes.ctl.lock().priority.is_empty() {
        return Ok((0, 0));
    }
    let mut cursor = i64::MAX;
    let (mut done, mut later) = (0usize, 0usize);
    let mut done_since_emit = 0usize;
    let mut told = Instant::now();
    loop {
        // Priority photos first, then the next of the pass.
        let first = {
            let mut c = dupes.ctl.lock();
            let id = c.priority.pop_front();
            if let Some(id) = id {
                c.priority_set.remove(&id);
            }
            id
        };
        let batch = match first {
            Some(id) => match needs(&*state.db.get()?, id)? {
                Some(row) => vec![row],
                None => continue,
            },
            None => {
                let b = next_batch(&*state.db.get()?, cursor)?;
                let Some(last) = b.last() else { break };
                cursor = last.id;
                b
            }
        };
        for row in &batch {
            yield_to_others(state);
            match hash_one(state, row)? {
                Outcome::Done => {
                    let mut s = dupes.status.write();
                    s.done += 1;
                    s.pending = (s.pending - 1).max(0);
                    done += 1;
                    done_since_emit += 1;
                }
                Outcome::Later => later += 1,
                Outcome::Gone => {}
            }
        }
        // An open report refreshes its counts now and then.
        if done_since_emit > 0 && told.elapsed() > Duration::from_secs(5) {
            state.emit(Event::Dupes);
            done_since_emit = 0;
            told = Instant::now();
        }
    }
    Ok((done, later))
}
