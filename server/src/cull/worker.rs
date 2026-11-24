//! The background worker: measures every photo outside the reject folders, newest first,
//! the photos on screen ahead of the rest. With People & pets on, a photo waits for its
//! people analysis (its faces are what is measured) and its faces' eyes are checked by
//! the AI engine; a photo measured without them is measured again once they come. It
//! steps aside whenever someone is waiting on an image and while the library is being
//! scanned.

use super::measure::{self, Subject};
use super::thresholds;
use crate::images::tone::{self, ToneMode};
use crate::model::{Kind, PhotoRec};
use crate::ml::{Client, MlError};
use crate::people::regions::Rect;
use crate::people::{FACE, IGNORED};
use crate::repo;
use crate::state::{AppState, Event};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

const BATCH: i64 = 8;
/// A photo that could not be read is tried again after this long (or once its file changes).
const RETRY_MS: i64 = 7 * 86_400_000;
/// Close-ups sent to the AI engine at once.
const EYES_BATCH: usize = 16;

/// Photos to measure (?1: retry errors from before, ?2: eyes can be checked, ?3: the people
/// analysis is running, so photos wait for it): outside the reject folders; never
/// measured, measured from another state of the file or of its faces, unreadable a while
/// ago, or measured without the eye checks that can now be made.
const NEEDS: &str = "p.pair_of IS NULL AND f.is_reject = 0
     AND (?3 = 0 OR a.photo_id IS NOT NULL)
     AND (c.photo_id IS NULL OR c.size != p.size OR c.mtime != p.mtime OR c.orientation != p.orientation
          OR c.faces_at != COALESCE(a.at, 0)
          OR (c.error IS NOT NULL AND c.at < ?1)
          OR (?2 = 1 AND c.eyes IS NULL AND c.error IS NULL
              AND EXISTS (SELECT 1 FROM cull_faces cf WHERE cf.photo_id = p.id)))";
const FROM: &str = "photos p JOIN folders f ON f.id = p.folder_id LEFT JOIN cull c ON c.photo_id = p.id LEFT JOIN analysis a ON a.photo_id = p.id";

pub fn spawn(state: Arc<AppState>) {
    std::thread::Builder::new()
        .name("cull".into())
        .spawn(move || {
            // Give the first scan and the thumbnails a head start.
            std::thread::sleep(Duration::from_secs(15));
            worker(&state)
        })
        .expect("spawn cull");
}

fn set_phase(state: &AppState, phase: &str, error: Option<String>) {
    let mut s = state.cull.status.write();
    s.phase = phase.into();
    s.error = error;
}

/// What the pass depends on right now.
#[derive(Clone, Copy, PartialEq)]
pub(super) struct Mode {
    pub eyes: bool,
    pub wait_for_people: bool,
}

fn args(mode: Mode) -> (i64, i64, i64) {
    (crate::db::now_ms() - RETRY_MS, mode.eyes as i64, mode.wait_for_people as i64)
}

fn counts(c: &Connection, mode: Mode) -> Result<(i64, i64)> {
    let total: i64 = c.query_row("SELECT count(*) FROM photos p JOIN folders f ON f.id = p.folder_id WHERE p.pair_of IS NULL AND f.is_reject = 0", [], |r| r.get(0))?;
    let (a, b, d) = args(mode);
    let todo: i64 = c.query_row(&format!("SELECT count(*) FROM {FROM} WHERE {NEEDS}"), params![a, b, d], |r| r.get(0))?;
    // Waiting for the people analysis counts as to do.
    let waiting: i64 = if mode.wait_for_people {
        c.query_row(
            "SELECT count(*) FROM photos p JOIN folders f ON f.id = p.folder_id LEFT JOIN analysis a ON a.photo_id = p.id
             WHERE p.pair_of IS NULL AND f.is_reject = 0 AND a.photo_id IS NULL",
            [],
            |r| r.get(0),
        )?
    } else {
        0
    };
    Ok(((total - todo - waiting).max(0), total))
}

fn update_counts(state: &AppState, mode: Mode) {
    if let Ok((done, total)) = state.db.get().map_err(anyhow::Error::from).and_then(|c| counts(&c, mode)) {
        let phase = {
            let mut s = state.cull.status.write();
            s.done = done;
            s.total = total;
            s.phase.clone()
        };
        state.emit(Event::Cull { phase, done, total, ids: Vec::new() });
    }
}

fn row(r: &rusqlite::Row) -> rusqlite::Result<PhotoRec> {
    Ok(PhotoRec { id: r.get(0)?, size: r.get(1)?, mtime: r.get(2)?, orientation: r.get(3)?, ..Default::default() })
}

/// The photos a pass measures (ids: 8 bytes each, a few MB for a whole library), newest
/// folders first: Year/Event paths sorted descending put 2026 before 2008, as the first
/// scan indexes them, and new imports go ahead of the archive. Within a folder, in the
/// order they were indexed. The photos are read in table order (CROSS JOIN keeps them
/// the outer loop) and the few that need work sorted after: a pass over a library with
/// nothing to do costs one sequential scan, as before.
fn pass_list(c: &Connection, mode: Mode) -> Result<VecDeque<i64>> {
    let from = FROM.replacen("JOIN folders", "CROSS JOIN folders", 1);
    let sql = format!("SELECT p.id FROM {from} WHERE {NEEDS} ORDER BY f.path DESC, p.id");
    let (a, b, d) = args(mode);
    let mut st = c.prepare(&sql)?;
    let rows = st.query_map(params![a, b, d], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<VecDeque<i64>>>()?)
}

/// The next photos of the pass that still need measuring, as they are now (one asked for
/// meanwhile is done; a changed one is taken as it is now).
fn next_batch(c: &Connection, mode: Mode, queue: &mut VecDeque<i64>) -> Vec<PhotoRec> {
    let mut b = Vec::new();
    while b.len() < BATCH as usize {
        let Some(id) = queue.pop_front() else { break };
        if let Ok(Some(now)) = needs(c, mode, id) {
            b.push(now);
        }
    }
    b
}

fn needs(c: &Connection, mode: Mode, id: i64) -> Result<Option<PhotoRec>> {
    let sql = format!("SELECT p.id, p.size, p.mtime, p.orientation FROM {FROM} WHERE p.id = ?4 AND {NEEDS}");
    let (a, b, d) = args(mode);
    Ok(c.prepare_cached(&sql)?.query_row(params![a, b, d, id], row).optional()?)
}

/// Wait while someone is looking at photos or the library is being scanned.
fn yield_to_others(state: &AppState) {
    let mut waited = false;
    loop {
        let scanning = state.progress.read().phase != "idle";
        if !state.images.busy() && !scanning {
            if waited {
                set_phase(state, "measuring", None);
            }
            break;
        }
        if scanning && !waited {
            set_phase(state, "waiting", None);
            waited = true;
        }
        std::thread::sleep(Duration::from_millis(if scanning { 1000 } else { 50 }));
    }
}

fn wait(state: &AppState, d: Duration) {
    let v = &state.cull;
    let mut c = v.ctl.lock();
    if !c.wake && c.priority.is_empty() {
        v.cv.wait_for(&mut c, d);
    }
    c.wake = false;
}

/// Whether eyes can be checked and whether photos wait for the people analysis.
fn current_mode(state: &AppState, client: Option<&Client>) -> Mode {
    let people = state.people.status.read().phase.clone();
    // While the models download, photos wait too: measured now, they would be measured
    // again once their faces are known.
    let wait_for_people = state.cfg.ml.faces_on() && matches!(people.as_str(), "analysing" | "idle" | "grouping" | "preparing");
    let eyes = client.and_then(|c| c.info().ok()).is_some_and(|i| i["models"]["eye_checker"].is_object());
    Mode { eyes, wait_for_people }
}

fn worker(state: &Arc<AppState>) {
    let lut = tone::build_lut(ToneMode::Normal, crate::images::RENDER_BOOST_EV);
    let mut rate: VecDeque<Instant> = VecDeque::new();
    let mut rx = state.events.subscribe();
    loop {
        // The engine's port changes when it restarts: ask for it each round.
        let client = if state.cfg.ml.eyes_on() { state.ml.client().ok() } else { None };
        let mode = current_mode(state, client.as_ref());
        state.cull.status.write().eyes = mode.eyes;
        update_counts(state, mode);
        let mut queue: Option<VecDeque<i64>> = None;
        let mut counted = Instant::now();
        let mut fresh: Vec<i64> = Vec::new();
        let mut told = Instant::now();
        let started = Instant::now();
        loop {
            // Photos asked for first, then the next of the pass.
            let first: Vec<PhotoRec> = {
                let ids: Vec<i64> = {
                    let mut c = state.cull.ctl.lock();
                    let n = c.priority.len().min(BATCH as usize);
                    c.priority.drain(..n).collect()
                };
                match state.db.get() {
                    Ok(c) => ids.into_iter().filter_map(|id| needs(&c, mode, id).ok().flatten()).collect(),
                    Err(_) => Vec::new(),
                }
            };
            let priority = !first.is_empty();
            let batch = if priority {
                first
            } else {
                match state.db.get() {
                    Ok(c) => {
                        let q = queue.get_or_insert_with(|| {
                            pass_list(&c, mode).unwrap_or_else(|e| {
                                tracing::warn!("cull: {e:#}");
                                VecDeque::new()
                            })
                        });
                        next_batch(&c, mode, q)
                    }
                    Err(e) => {
                        tracing::warn!("cull: {e:#}");
                        Vec::new()
                    }
                }
            };
            if batch.is_empty() {
                tell(state, &mut fresh);
                update_counts(state, mode);
                set_phase(state, "idle", None);
                state.cull.status.write().rate = 0.0;
                // Until something changes: on disk, in the people analysis, or asked for.
                let until = Instant::now() + Duration::from_secs(if mode.wait_for_people { 120 } else { 1800 });
                loop {
                    let mut changed = false;
                    while let Ok(e) = rx.try_recv() {
                        if matches!(e, Event::Folder { .. } | Event::Tree | Event::People { .. }) {
                            changed = true;
                        }
                    }
                    if changed || Instant::now() >= until || !state.cull.ctl.lock().priority.is_empty() {
                        break;
                    }
                    wait(state, Duration::from_secs(30));
                }
                break;
            }
            set_phase(state, "measuring", None);
            let mut offline = false;
            for rec in &batch {
                if !priority {
                    yield_to_others(state);
                }
                match one(state, client.as_ref(), mode, &lut, rec) {
                    Ok(true) => {
                        fresh.push(rec.id);
                        let now = Instant::now();
                        rate.push_back(now);
                        while rate.front().is_some_and(|t| now.duration_since(*t) > Duration::from_secs(300)) {
                            rate.pop_front();
                        }
                        let r = if rate.len() > 1 { rate.len() as f64 / now.duration_since(rate[0]).as_secs_f64().max(1.0) * 60.0 } else { 0.0 };
                        state.cull.status.write().rate = r;
                    }
                    Ok(false) => {}
                    Err(e) => {
                        // The AI engine went away mid-way: measure again later.
                        set_phase(state, "offline", Some(format!("{e:#}")));
                        offline = true;
                        break;
                    }
                }
                if told.elapsed() > Duration::from_secs(2) {
                    tell(state, &mut fresh);
                    told = Instant::now();
                }
            }
            if offline {
                tell(state, &mut fresh);
                wait(state, Duration::from_secs(60));
                break;
            }
            if counted.elapsed() > Duration::from_secs(15) {
                update_counts(state, mode);
                counted = Instant::now();
            }
            // The AI engine or the people analysis may have changed: look again now
            // and then.
            if started.elapsed() > Duration::from_secs(600) && !priority {
                break;
            }
        }
    }
}

/// Tells the viewers which photos have new measurements.
fn tell(state: &AppState, fresh: &mut Vec<i64>) {
    if fresh.is_empty() {
        return;
    }
    let s = state.cull.status.read().clone();
    let ids = std::mem::take(fresh);
    state.emit(Event::Cull { phase: s.phase, done: s.done, total: s.total, ids });
}

fn subjects(c: &Connection, photo: i64) -> Result<Vec<Subject>> {
    let sql = format!("SELECT id, kind, x, y, w, h FROM detections WHERE photo_id = ?1 AND state != ?2 AND (kind != 1 OR {}) ORDER BY id", super::REAL_FACE);
    let mut st = c.prepare_cached(&sql)?;
    let rows = st.query_map(params![photo, IGNORED], |r| {
        Ok(Subject { det: r.get(0)?, kind: r.get(1)?, rect: Rect { x: r.get(2)?, y: r.get(3)?, w: r.get(4)?, h: r.get(5)? } })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

struct Eye {
    closed: f64,
    at: [[f32; 2]; 2],
}

/// Measures a photo and keeps the result. Ok(false): nothing to do (gone, rejected);
/// an error: the AI engine is away (measure it again later).
pub(super) fn one(state: &AppState, client: Option<&Client>, mode: Mode, lut: &[u8], rec: &PhotoRec) -> Result<bool> {
    let c = state.db.get()?;
    let Some(loc) = repo::load(&c, rec.id)? else { return Ok(false) };
    if loc.folder_is_reject {
        return Ok(false);
    }
    let faces_at: i64 = c.query_row("SELECT at FROM analysis WHERE photo_id = ?1", [rec.id], |r| r.get(0)).optional()?.unwrap_or(0);
    let subs = subjects(&c, rec.id)?;
    drop(c);
    let src = crate::scan::photo_path(state, &loc.folder_path, &loc.rec.name);
    let kind = Kind::from_i32(loc.rec.kind);
    let image = match measure::load(&src, kind, loc.rec.orientation as u8, lut) {
        Ok(i) => i,
        Err(e) => {
            save_error(state, &loc.rec, faces_at, &format!("{e:#}"))?;
            return Ok(true);
        }
    };
    let img = &image.img;
    let focus: Vec<(Subject, Option<f64>)> = subs.iter().map(|s| (*s, measure::subject_focus(img, s.rect))).collect();
    let (subject_focus, focus_on) = measure::subject(&focus, || measure::scene_focus(img));
    // Eyes.
    let mut eyes: std::collections::HashMap<i64, Eye> = std::collections::HashMap::new();
    let (checked, closed) = match client.filter(|_| mode.eyes) {
        None => (None, None),
        Some(client) => {
            let crops: Vec<(i64, Vec<u8>, [f64; 3])> =
                subs.iter().filter(|s| s.kind == FACE).filter_map(|s| measure::eye_crop(img, s.rect).map(|(d, at)| (s.det, d, at))).collect();
            let mut checked = 0i64;
            let mut closed = 0i64;
            for part in crops.chunks(EYES_BATCH) {
                let data: Vec<Vec<u8>> = part.iter().map(|p| p.1.clone()).collect();
                match client.check_eyes(&data) {
                    Ok(res) => {
                        for ((det, _, at), r) in part.iter().zip(res) {
                            // Not clearly a face (a mask, the back of a head): no verdict.
                            let Some(r) = r.filter(|r| r.p >= 0.5) else { continue };
                            checked += 1;
                            if r.closed as f64 >= thresholds().closed {
                                closed += 1;
                            }
                            let pos = |k: usize| r.eyes.get(k).map(|p| measure::from_crop(img, *at, *p)).unwrap_or([0.0, 0.0]);
                            eyes.insert(*det, Eye { closed: r.closed as f64, at: [pos(0), pos(1)] });
                        }
                    }
                    Err(MlError::Unreachable(e) | MlError::Preparing(e)) => anyhow::bail!("the AI engine: {e}"),
                    Err(MlError::Rejected(e)) => tracing::debug!("cull: eyes of photo {}: {e}", rec.id),
                }
            }
            (Some(checked), Some(closed))
        }
    };
    let issues = super::issues(subject_focus, closed, Some(image.over), Some(image.under));
    let mut conn = state.db.get()?;
    let tx = conn.transaction()?;
    // The photo may have changed or gone meanwhile.
    let now: Option<(i64, i64, i32)> =
        tx.query_row("SELECT size, mtime, orientation FROM photos WHERE id = ?1", [rec.id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional()?;
    if now != Some((loc.rec.size, loc.rec.mtime, loc.rec.orientation)) {
        return Ok(false);
    }
    tx.execute(
        "INSERT INTO cull(photo_id, size, mtime, orientation, faces_at, at, error, focus, focus_on, over, under, eyes, closed, issues)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT(photo_id) DO UPDATE SET size = excluded.size, mtime = excluded.mtime, orientation = excluded.orientation,
            faces_at = excluded.faces_at, at = excluded.at, error = NULL, focus = excluded.focus, focus_on = excluded.focus_on,
            over = excluded.over, under = excluded.under, eyes = excluded.eyes, closed = excluded.closed, issues = excluded.issues",
        params![
            rec.id,
            loc.rec.size,
            loc.rec.mtime,
            loc.rec.orientation,
            faces_at,
            crate::db::now_ms(),
            subject_focus,
            focus_on,
            image.over,
            image.under,
            checked,
            closed,
            issues
        ],
    )?;
    tx.execute("DELETE FROM cull_faces WHERE photo_id = ?1", [rec.id])?;
    for (s, f) in &focus {
        let e = eyes.get(&s.det);
        if f.is_none() && e.is_none() {
            continue;
        }
        let at = e.map(|e| serde_json::to_string(&e.at).unwrap_or_default());
        tx.execute(
            "INSERT OR REPLACE INTO cull_faces(detection_id, photo_id, focus, closed, eyes) VALUES(?1, ?2, ?3, ?4, ?5)",
            params![s.det, rec.id, f, e.map(|e| e.closed), at],
        )?;
    }
    tx.commit()?;
    Ok(true)
}

fn save_error(state: &AppState, rec: &PhotoRec, faces_at: i64, err: &str) -> Result<()> {
    let c = state.db.get()?;
    c.execute(
        "INSERT INTO cull(photo_id, size, mtime, orientation, faces_at, at, error, issues) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, 0)
         ON CONFLICT(photo_id) DO UPDATE SET size = excluded.size, mtime = excluded.mtime, orientation = excluded.orientation,
            faces_at = excluded.faces_at, at = excluded.at, error = excluded.error, focus = NULL, focus_on = NULL, over = NULL,
            under = NULL, eyes = NULL, closed = NULL, issues = 0",
        params![rec.id, rec.size, rec.mtime, rec.orientation, faces_at, crate::db::now_ms(), err.chars().take(300).collect::<String>()],
    )?;
    c.execute("DELETE FROM cull_faces WHERE photo_id = ?1", [rec.id])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pass_goes_newest_folders_first() {
        let t = tempfile::tempdir().unwrap();
        let pool = crate::db::open(&t.path().join("i.db")).unwrap();
        let c = pool.get().unwrap();
        // Indexed oldest first, as an older index or a library that grew over the years.
        c.execute_batch(
            "INSERT INTO folders(id, parent_id, path, name) VALUES(1, NULL, '', 'Library'), (2, 1, '2008/Trip', 'Trip'),
                (3, 1, '2017/Lake', 'Lake'), (4, 1, '2026/01 New', 'New');
             INSERT INTO photos(id, folder_id, name, stem, ext, kind, size, mtime) VALUES
                (1, 2, 'A.CR2', 'A', 'cr2', 1, 1, 1), (2, 2, 'B.CR2', 'B', 'cr2', 1, 1, 1),
                (3, 3, 'C.CR2', 'C', 'cr2', 1, 1, 1), (4, 4, 'D.CR3', 'D', 'cr3', 1, 1, 1), (5, 4, 'E.CR3', 'E', 'cr3', 1, 1, 1);
             INSERT INTO cull(photo_id, size, mtime, orientation, at) VALUES(3, 1, 1, 1, 1);",
        )
        .unwrap();
        let mode = Mode { eyes: false, wait_for_people: false };
        let mut q = pass_list(&c, mode).unwrap();
        let ids: Vec<i64> = q.iter().copied().collect();
        // C is measured already.
        assert_eq!(ids, vec![4, 5, 1, 2]);
        // Measured meanwhile (asked for): skipped when its turn comes.
        c.execute("INSERT INTO cull(photo_id, size, mtime, orientation, at) VALUES(5, 1, 1, 1, 1)", []).unwrap();
        let b: Vec<i64> = next_batch(&c, mode, &mut q).iter().map(|r| r.id).collect();
        assert_eq!(b, vec![4, 1, 2]);
        assert!(q.is_empty());
    }
}
