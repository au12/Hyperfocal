//! The background worker: an image embedding of every photo shown in lists, made by the
//! ML service from the photo's thumbnail, newest photos first; the photos on screen go
//! ahead of everything. It steps aside whenever someone is waiting on an image and
//! while the library is being scanned, and waits for the ML service when it is away.

use super::{Index, store};
use crate::model::PhotoRec;
use crate::people::ml::{Client, MlError};
use crate::repo;
use crate::state::{AppState, Event};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Photos sent to the ML service at once.
const BATCH: i64 = 16;
/// A photo that could not be read is tried again after this long (or once its file changes).
const RETRY_MS: i64 = 7 * 86_400_000;

/// Photos that need a (new) embedding: shown ones outside the reject folders, never
/// embedded, embedded from another state of the file, or unreadable a while ago.
const NEEDS: &str = "p.pair_of IS NULL AND f.is_reject = 0
     AND (c.photo_id IS NULL OR c.size != p.size OR c.mtime != p.mtime OR c.orientation != p.orientation
          OR (c.emb IS NULL AND c.at < ?1))";

pub fn spawn(state: Arc<AppState>) {
    if state.cfg.ml_url.is_none() {
        return;
    }
    set_phase(&state, "connecting", None);
    std::thread::Builder::new()
        .name("visual".into())
        .spawn(move || {
            match state.db.get().map_err(anyhow::Error::from).and_then(|c| Index::load(&c)) {
                Ok(ix) => *state.visual.index.write() = ix,
                Err(e) => tracing::warn!("search: loading the embeddings: {e:#}"),
            }
            // Give the first scan, the thumbnails and the people analysis a head start.
            std::thread::sleep(Duration::from_secs(20));
            worker(&state)
        })
        .expect("spawn visual");
}

fn set_phase(state: &AppState, phase: &str, error: Option<String>) {
    let mut s = state.visual.status.write();
    s.phase = phase.into();
    s.error = error;
}

pub fn counts(c: &Connection) -> Result<(i64, i64)> {
    let total: i64 = c.query_row("SELECT count(*) FROM photos p JOIN folders f ON f.id = p.folder_id WHERE p.pair_of IS NULL AND f.is_reject = 0", [], |r| r.get(0))?;
    let todo: i64 = c.query_row(
        &format!("SELECT count(*) FROM photos p JOIN folders f ON f.id = p.folder_id LEFT JOIN clip c ON c.photo_id = p.id WHERE {NEEDS}"),
        [crate::db::now_ms() - RETRY_MS],
        |r| r.get(0),
    )?;
    Ok(((total - todo).max(0), total))
}

fn update_counts(state: &AppState) {
    if let Ok((done, total)) = state.db.get().map_err(anyhow::Error::from).and_then(|c| counts(&c)) {
        let phase = {
            let mut s = state.visual.status.write();
            s.done = done;
            s.total = total;
            s.phase.clone()
        };
        state.emit(Event::Visual { phase, done, total });
    }
}

fn row(r: &rusqlite::Row) -> rusqlite::Result<PhotoRec> {
    Ok(PhotoRec { id: r.get(0)?, size: r.get(1)?, mtime: r.get(2)?, orientation: r.get(3)?, ..Default::default() })
}

/// The photos a pass looks at (ids: 8 bytes each, a few MB for a whole library), newest
/// folders first: Year/Event paths sorted descending put 2026 before 2008, as the first
/// scan indexes them, and new imports go ahead of the archive. Within a folder, in the
/// order they were indexed. The photos are read in table order (CROSS JOIN keeps them
/// the outer loop) and the few that need work sorted after: a pass over a library with
/// nothing to do costs one sequential scan.
fn pass_list(c: &Connection) -> Result<VecDeque<i64>> {
    let sql = format!(
        "SELECT p.id FROM photos p CROSS JOIN folders f ON f.id = p.folder_id
         LEFT JOIN clip c ON c.photo_id = p.id WHERE {NEEDS} ORDER BY f.path DESC, p.id"
    );
    let mut st = c.prepare(&sql)?;
    let rows = st.query_map(params![crate::db::now_ms() - RETRY_MS], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<VecDeque<i64>>>()?)
}

/// The next photos of the pass that still need an embedding, as they are now (one asked
/// for meanwhile is done; a changed one is taken as it is now).
fn next_batch(c: &Connection, queue: &mut VecDeque<i64>) -> Vec<PhotoRec> {
    let mut b = Vec::new();
    while b.len() < BATCH as usize {
        let Some(id) = queue.pop_front() else { break };
        if let Ok(Some(now)) = needs(c, id) {
            b.push(now);
        }
    }
    b
}

fn needs(c: &Connection, id: i64) -> Result<Option<PhotoRec>> {
    let sql = format!(
        "SELECT p.id, p.size, p.mtime, p.orientation FROM photos p JOIN folders f ON f.id = p.folder_id
         LEFT JOIN clip c ON c.photo_id = p.id WHERE p.id = ?2 AND {NEEDS}"
    );
    Ok(c.prepare_cached(&sql)?.query_row(params![crate::db::now_ms() - RETRY_MS, id], row).optional()?)
}

/// Wait while someone is looking at photos or the library is being scanned.
fn yield_to_others(state: &AppState) {
    let mut waited = false;
    loop {
        let scanning = state.progress.read().phase != "idle";
        if !state.images.busy() && !scanning {
            if waited {
                set_phase(state, "indexing", None);
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
    let v = &state.visual;
    let mut c = v.ctl.lock();
    if !c.wake && c.priority.is_empty() {
        v.cv.wait_for(&mut c, d);
    }
    c.wake = false;
}

/// When the ML service brings another image model, the stored embeddings mean nothing
/// to it: they are dropped and made again.
fn check_model(state: &AppState, id: &str) -> Result<()> {
    let c = state.db.get()?;
    let old = crate::db::get_meta(&c, "visual_model");
    if old.as_deref() == Some(id) {
        return Ok(());
    }
    if let Some(old) = old {
        let n = c.execute("DELETE FROM clip", [])?;
        *state.visual.index.write() = Index::default();
        tracing::warn!("search: the image model changed ({old} -> {id}): {n} photos will be looked at again");
    }
    crate::db::set_meta(&c, "visual_model", id)?;
    Ok(())
}

fn worker(state: &Arc<AppState>) {
    let url = state.cfg.ml_url.clone().unwrap_or_default();
    let client = Client::new(&url, state.cfg.ml_token.clone());
    let mut backoff = Duration::from_secs(5);
    let mut rate: VecDeque<Instant> = VecDeque::new();
    let mut rx = state.events.subscribe();
    loop {
        let info = match client.info() {
            Ok(i) => i,
            Err(e) => {
                set_phase(state, "offline", Some(format!("{e:#}")));
                wait(state, backoff);
                backoff = (backoff * 2).min(Duration::from_secs(120));
                continue;
            }
        };
        backoff = Duration::from_secs(5);
        let Some(model) = info["models"]["image_embedder"]["id"].as_str().map(str::to_string) else {
            // An ML service without the search models (an older image, or HFML_SEARCH=0).
            set_phase(state, "unsupported", Some("the ML service has no search models (update hyperfocal-ml, or HFML_SEARCH=0 is set)".into()));
            wait(state, Duration::from_secs(600));
            continue;
        };
        if let Err(e) = check_model(state, &model) {
            tracing::warn!("search: {e:#}");
        }
        update_counts(state);
        // Work until the service goes away.
        let mut queue: Option<VecDeque<i64>> = None;
        let mut counted = Instant::now();
        loop {
            // Photos asked for first, then the next of the pass.
            let first: Vec<PhotoRec> = {
                let ids: Vec<i64> = {
                    let mut c = state.visual.ctl.lock();
                    let n = c.priority.len().min(BATCH as usize);
                    c.priority.drain(..n).collect()
                };
                match state.db.get() {
                    Ok(c) => ids.into_iter().filter_map(|id| needs(&c, id).ok().flatten()).collect(),
                    Err(_) => Vec::new(),
                }
            };
            let batch = if !first.is_empty() {
                first
            } else {
                match state.db.get() {
                    Ok(c) => {
                        let q = queue.get_or_insert_with(|| {
                            pass_list(&c).unwrap_or_else(|e| {
                                tracing::warn!("search: {e:#}");
                                VecDeque::new()
                            })
                        });
                        next_batch(&c, q)
                    }
                    Err(e) => {
                        tracing::warn!("search: {e:#}");
                        Vec::new()
                    }
                }
            };
            if batch.is_empty() {
                update_counts(state);
                set_phase(state, "idle", None);
                state.visual.status.write().rate = 0.0;
                // Until something changes on disk, or now and then.
                let until = Instant::now() + Duration::from_secs(1800);
                loop {
                    let mut changed = false;
                    while let Ok(e) = rx.try_recv() {
                        if matches!(e, Event::Folder { .. } | Event::Tree) {
                            changed = true;
                        }
                    }
                    if changed || Instant::now() >= until || !state.visual.ctl.lock().priority.is_empty() {
                        break;
                    }
                    wait(state, Duration::from_secs(30));
                }
                // The service may have changed (another model): check again.
                break;
            }
            set_phase(state, "indexing", None);
            match embed_batch(state, &client, &batch) {
                Ok(n) => {
                    let now = Instant::now();
                    for _ in 0..n {
                        rate.push_back(now);
                    }
                    while rate.front().is_some_and(|t| now.duration_since(*t) > Duration::from_secs(300)) {
                        rate.pop_front();
                    }
                    let r = if rate.len() > 1 { rate.len() as f64 / now.duration_since(rate[0]).as_secs_f64().max(1.0) * 60.0 } else { 0.0 };
                    state.visual.status.write().rate = r;
                }
                Err(e) => {
                    set_phase(state, "offline", Some(format!("{e:#}")));
                    wait(state, Duration::from_secs(30));
                    break;
                }
            }
            if counted.elapsed() > Duration::from_secs(15) {
                update_counts(state);
                counted = Instant::now();
            }
        }
    }
}

/// Embeds a batch: thumbnails to the ML service, embeddings to the index. Returns how
/// many were stored; an error when the service failed (try again later).
fn embed_batch(state: &AppState, client: &Client, batch: &[PhotoRec]) -> Result<usize> {
    let mut recs = Vec::with_capacity(batch.len());
    let mut images = Vec::with_capacity(batch.len());
    for r in batch {
        yield_to_others(state);
        let loc = {
            let c = state.db.get()?;
            match repo::load(&c, r.id)? {
                Some(l) if !l.folder_is_reject => l,
                _ => continue,
            }
        };
        match crate::images::thumb_jpeg(state, &loc) {
            Ok(jpeg) => {
                images.push(jpeg);
                recs.push(loc.rec);
            }
            Err(e) => {
                let c = state.db.get()?;
                let _ = store(state, &c, &loc.rec, None, Some(&format!("{e:#}")));
            }
        }
    }
    if images.is_empty() {
        return Ok(0);
    }
    let embs = match client.embed_images(&images) {
        Ok(e) => e,
        Err(MlError::Rejected(msg)) => {
            // One bad request: mark the batch, so the pass moves on.
            let c = state.db.get()?;
            for r in &recs {
                let _ = store(state, &c, r, None, Some(&msg));
            }
            return Ok(0);
        }
        Err(MlError::Unreachable(msg)) => return Err(anyhow::anyhow!(msg)),
    };
    let mut c = state.db.get()?;
    let tx = c.transaction()?;
    let mut n = 0;
    for (r, e) in recs.iter().zip(embs) {
        // A photo deleted meanwhile (foreign key) is skipped.
        match &e {
            Some(v) => {
                if store(state, &tx, r, Some(v), None).is_ok() {
                    n += 1;
                }
            }
            None => {
                let _ = store(state, &tx, r, None, Some("the ML service could not read the thumbnail"));
            }
        }
    }
    tx.commit()?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pass_goes_newest_folders_first() {
        let t = tempfile::tempdir().unwrap();
        let pool = crate::db::open(&t.path().join("i.db")).unwrap();
        let c = pool.get().unwrap();
        c.execute_batch(
            "INSERT INTO folders(id, parent_id, path, name) VALUES(1, NULL, '', 'Library'), (2, 1, '2008/Trip', 'Trip'), (3, 1, '2026/01 New', 'New');
             INSERT INTO photos(id, folder_id, name, stem, ext, kind, size, mtime) VALUES
                (1, 2, 'A.CR2', 'A', 'cr2', 1, 1, 1), (2, 3, 'B.CR3', 'B', 'cr3', 1, 1, 1), (3, 3, 'C.CR3', 'C', 'cr3', 1, 1, 1);",
        )
        .unwrap();
        let mut q = pass_list(&c).unwrap();
        assert_eq!(q.iter().copied().collect::<Vec<_>>(), vec![2, 3, 1]);
        let b: Vec<i64> = next_batch(&c, &mut q).iter().map(|r| r.id).collect();
        assert_eq!(b, vec![2, 3, 1]);
    }
}
