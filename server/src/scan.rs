//! Library scanner: mirrors the folder tree into the index and reads metadata of new
//! or changed files. Reading only — the scanner never writes to the library.

use crate::meta;
use crate::model::{Kind, PhotoRec, kind_of_ext, split_name};
use crate::repo;
use crate::sets::{self, Listing, Plan};
use crate::state::{AppState, Event, ScanMsg};
use anyhow::{Context, Result};
use rayon::prelude::*;
use rusqlite::{OptionalExtension, params};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, UNIX_EPOCH};

const SKIP_DIRS: &[&str] = &["@eaDir", "#recycle", "#snapshot", "$RECYCLE.BIN", "System Volume Information", "lost+found"];

pub fn is_skipped_name(name: &str) -> bool {
    name.starts_with('.') || SKIP_DIRS.iter().any(|s| s.eq_ignore_ascii_case(name)) || name.starts_with(".Trash")
}

pub fn is_reject_name(state: &AppState, name: &str) -> bool {
    name.eq_ignore_ascii_case(&state.cfg.reject_dir) || name.eq_ignore_ascii_case("_rejected")
}

pub fn abs_dir(state: &AppState, rel: &str) -> PathBuf {
    if rel.is_empty() { state.cfg.photos_root.clone() } else { state.cfg.photos_root.join(rel) }
}

pub(crate) fn mtime_ms(md: &std::fs::Metadata) -> i64 {
    md.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_millis() as i64).unwrap_or(0)
}

struct WalkedDir {
    rel: String,
    name: String,
    parent: Option<String>,
}

/// Walk the folder tree and sync the `folders` table. Returns false when the library
/// looks unavailable (nothing is pruned in that case).
pub fn sync_tree(state: &AppState) -> Result<bool> {
    let root = &state.cfg.photos_root;
    let mut dirs = vec![WalkedDir { rel: String::new(), name: String::new(), parent: None }];
    let mut failed: Vec<String> = Vec::new();
    let mut stack = vec![String::new()];
    let mut root_entries = 0usize;
    while let Some(rel) = stack.pop() {
        let abs = if rel.is_empty() { root.clone() } else { root.join(&rel) };
        let rd = match std::fs::read_dir(&abs) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("cannot read {}: {e}", abs.display());
                failed.push(rel.clone());
                continue;
            }
        };
        for ent in rd.flatten() {
            if rel.is_empty() {
                root_entries += 1;
            }
            let Ok(ft) = ent.file_type() else { continue };
            if !ft.is_dir() {
                continue;
            }
            let Some(name) = ent.file_name().to_str().map(|s| s.to_string()) else { continue };
            if is_skipped_name(&name) {
                continue;
            }
            let child = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
            dirs.push(WalkedDir { rel: child.clone(), name, parent: Some(rel.clone()) });
            stack.push(child);
        }
    }

    let mut conn = state.db.get()?;
    let existing: i64 = conn.query_row("SELECT count(*) FROM photos", [], |r| r.get(0))?;
    let root_ok = !failed.iter().any(|f| f.is_empty()) && (root_entries > 0 || existing == 0);
    // Parents before children.
    dirs.sort_by(|a, b| a.rel.matches('/').count().cmp(&b.rel.matches('/').count()).then(a.rel.cmp(&b.rel)));

    let tx = conn.transaction()?;
    let mut ids: HashMap<String, i64> = HashMap::new();
    let mut changed = false;
    {
        tx.execute("UPDATE folders SET seen = 0", [])?;
        let mut sel = tx.prepare_cached("SELECT id, parent_id FROM folders WHERE path = ?1")?;
        let mut ins = tx.prepare_cached(
            "INSERT INTO folders(parent_id, path, name, is_reject, seen) VALUES(?1, ?2, ?3, ?4, 1) RETURNING id",
        )?;
        let mut upd = tx.prepare_cached("UPDATE folders SET seen = 1, parent_id = ?2, is_reject = ?3 WHERE id = ?1")?;
        for d in &dirs {
            let parent_id = d.parent.as_ref().and_then(|p| ids.get(p).copied());
            let is_rej = !d.rel.is_empty() && is_reject_name(state, &d.name);
            let found: Option<(i64, Option<i64>)> = sel.query_row([&d.rel], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
            let id = match found {
                Some((id, _)) => {
                    upd.execute(params![id, parent_id, is_rej as i64])?;
                    id
                }
                None => {
                    changed = true;
                    let name = if d.rel.is_empty() { "Library".to_string() } else { d.name.clone() };
                    ins.query_row(params![parent_id, d.rel, name, is_rej as i64], |r| r.get(0))?
                }
            };
            ids.insert(d.rel.clone(), id);
        }
        // Keep everything below folders that could not be read.
        for f in &failed {
            tx.execute(
                "UPDATE folders SET seen = 1 WHERE ?1 = '' OR path = ?1 OR substr(path, 1, length(?1) + 1) = ?1 || '/'",
                [f],
            )?;
        }
        if root_ok {
            let n = tx.execute("DELETE FROM folders WHERE seen = 0", [])?;
            if n > 0 {
                changed = true;
                // Remove orphaned full-text rows.
                tx.execute("DELETE FROM photos_fts WHERE rowid NOT IN (SELECT id FROM photos)", [])?;
                tx.execute("DELETE FROM photos_tri WHERE rowid NOT IN (SELECT id FROM photos)", [])?;
            }
        }
    }
    tx.commit()?;
    if changed {
        state.emit(Event::Tree);
    }
    Ok(root_ok)
}

struct FileEntry {
    name: String,
    stem: String,
    ext: String,
    kind: Kind,
    size: i64,
    mtime: i64,
}

/// Give darktable/digiKam-style sidecars (`IMG_1.CR3.xmp`) the standard name
/// (`IMG_1.xmp`, which Excire and Lightroom read) where `sets` allows it: for every
/// photo in the folder, or only `only`. Never overwrites, skips renames that were
/// undone, and journals each one. Returns the `(from, to)` renames done.
pub fn standardize_sidecars(
    state: &AppState,
    folder_path: &str,
    dir: &Path,
    plan: &Plan,
    ids: &HashMap<&str, i64>,
    only: Option<&str>,
) -> Vec<(String, String)> {
    if !state.cfg.rename_sidecars || state.is_readonly() {
        return Vec::new();
    }
    let mut done = Vec::new();
    let mut batch: Option<String> = None;
    for (photo, from, to) in plan.standard_renames() {
        if only.is_some_and(|o| o != photo) || rename_was_undone(state, folder_path, &from) {
            continue;
        }
        if let Err(e) = crate::files::rename_noreplace(&dir.join(&from), &dir.join(&to)) {
            tracing::warn!("could not rename {} to {to}: {e}", dir.join(&from).display());
            continue;
        }
        tracing::info!("renamed {} to {to}", dir.join(&from).display());
        let batch = batch.get_or_insert_with(crate::ops::new_batch);
        let data = serde_json::json!({ "folder": folder_path, "photo": photo, "from": from, "to": to });
        if let Err(e) = crate::ops::journal(state, batch, "rename", ids.get(photo).copied(), "renamed sidecars to the standard name", &data) {
            tracing::warn!("journaling the rename of {from}: {e:#}");
        }
        done.push((from, to));
    }
    done
}

fn rename_was_undone(state: &AppState, folder_path: &str, from: &str) -> bool {
    let Ok(c) = state.db.get() else { return true };
    c.query_row(
        "SELECT 1 FROM journal WHERE kind = 'rename' AND undone = 1
            AND json_extract(data, '$.folder') = ?1 AND json_extract(data, '$.from') = ?2 LIMIT 1",
        params![folder_path, from],
        |_| Ok(()),
    )
    .is_ok()
}

/// The files of a library folder and of the folders it shares sidecars with (see
/// `sets`), read fresh from disk.
pub fn listing(state: &AppState, folder_path: &str, is_reject: bool) -> Result<Listing> {
    let dir = abs_dir(state, folder_path);
    Listing::read(&dir, is_reject, &|n: &str| is_reject_name(state, n)).with_context(|| format!("reading {}", dir.display()))
}

pub struct FolderScan {
    pub changed: usize,
    pub removed: usize,
    pub total: usize,
}

/// Re-read one folder: new, changed and removed files and sidecars.
pub fn scan_folder(state: &AppState, folder_id: i64, force: bool) -> Result<FolderScan> {
    state.with_folder_lock(folder_id, || scan_folder_locked(state, folder_id, force))
}

fn scan_folder_locked(state: &AppState, folder_id: i64, force: bool) -> Result<FolderScan> {
    let conn = state.db.get()?;
    let (rel, is_reject): (String, bool) =
        conn.query_row("SELECT path, is_reject FROM folders WHERE id = ?1", [folder_id], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? != 0)))?;
    drop(conn);
    let dir = abs_dir(state, &rel);
    let rd = std::fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))?;

    let mut files: Vec<FileEntry> = Vec::new();
    let mut sidecars: HashMap<String, (String, i64)> = HashMap::new();
    let mut names: Vec<String> = Vec::new();
    let mut rejects: Vec<PathBuf> = Vec::new();
    for ent in rd.flatten() {
        let Some(name) = ent.file_name().to_str().map(|s| s.to_string()) else { continue };
        if name.starts_with('.') {
            continue;
        }
        let Ok(md) = ent.metadata() else { continue };
        if md.is_dir() && is_reject_name(state, &name) {
            rejects.push(dir.join(&name));
        }
        if !md.is_file() {
            continue;
        }
        names.push(name.clone());
        let (stem, ext) = split_name(&name);
        if ext == "xmp" {
            sidecars.insert(name.to_lowercase(), (name.clone(), mtime_ms(&md)));
            continue;
        }
        if let Some(kind) = kind_of_ext(&ext) {
            files.push(FileEntry { name, stem, ext, kind, size: md.len() as i64, mtime: mtime_ms(&md) });
        }
    }
    // Which sidecar each photo reads depends on the photos with the same name here and
    // in the reject (or home) folder.
    let partners = sets::partner_names(&dir, is_reject, &rejects, &|n: &str| is_reject_name(state, n)).unwrap_or_else(|e| {
        tracing::warn!("reading the folders next to {}: {e}", dir.display());
        Vec::new()
    });
    let mut listing = Listing::new(names, partners);

    let conn = state.db.get()?;
    let mut existing: HashMap<String, (i64, i64, i64, Option<String>, Option<i64>)> = HashMap::new();
    {
        let mut st = conn.prepare("SELECT id, name, size, mtime, sidecar, sidecar_mtime FROM photos WHERE folder_id = ?1")?;
        let rows = st.query_map([folder_id], |r| {
            Ok((r.get::<_, String>(1)?, (r.get(0)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))
        })?;
        for r in rows {
            let (n, v) = r?;
            existing.insert(n, v);
        }
    }
    drop(conn);

    let ids: HashMap<&str, i64> = existing.iter().map(|(n, v)| (n.as_str(), v.0)).collect();
    let renamed = standardize_sidecars(state, &rel, &dir, &Plan::new(&listing), &ids, None);
    for (from, to) in &renamed {
        listing.rename(from, to);
        if let Some((_, mtime)) = sidecars.remove(&from.to_lowercase()) {
            sidecars.insert(to.to_lowercase(), (to.clone(), mtime));
        }
    }
    let plan = Plan::new(&listing);

    let mut todo: Vec<(FileEntry, Option<(String, i64)>)> = Vec::new();
    let present: HashSet<String> = files.iter().map(|f| f.name.clone()).collect();
    for f in files {
        let sc = plan.read_sidecar(&f.name).and_then(|n| sidecars.get(&n.to_lowercase()).cloned());
        let unchanged = match existing.get(&f.name) {
            Some((_, size, mtime, sname, smtime)) => {
                *size == f.size
                    && *mtime == f.mtime
                    && sname.as_deref() == sc.as_ref().map(|s| s.0.as_str())
                    && *smtime == sc.as_ref().map(|s| s.1)
            }
            None => false,
        };
        if force || !unchanged {
            todo.push((f, sc));
        }
    }
    let removed: Vec<i64> =
        existing.iter().filter(|(n, _)| !present.contains(*n)).map(|(_, v)| v.0).collect();
    let total = present.len();

    let recs: Vec<PhotoRec> = state.images.index_pool.install(|| {
        todo.par_iter()
            .map(|(f, sc)| {
                let path = dir.join(&f.name);
                let mut rec = PhotoRec {
                    folder_id,
                    name: f.name.clone(),
                    stem: f.stem.clone(),
                    ext: f.ext.clone(),
                    kind: f.kind as i32,
                    size: f.size,
                    mtime: f.mtime,
                    sidecar: sc.as_ref().map(|s| s.0.clone()),
                    sidecar_mtime: sc.as_ref().map(|s| s.1),
                    orientation: 1,
                    ..Default::default()
                };
                let sidecar_xmp = sc.as_ref().and_then(|s| meta::read_sidecar(&dir.join(&s.0)));
                // A parser that panics on one malformed file must not stop the scanner.
                match crate::images::raw::catch(|| meta::read_file_meta(&path, f.kind)) {
                    Ok(fm) => meta::merge_into(&mut rec, &fm, sidecar_xmp.as_ref()),
                    Err(e) => {
                        rec.meta_err = Some(e);
                        meta::merge_into(&mut rec, &meta::FileMeta { orientation: 1, ..Default::default() }, sidecar_xmp.as_ref());
                    }
                }
                rec
            })
            .collect()
    });

    let changed = recs.len();
    if changed > 0 || !removed.is_empty() {
        let mut conn = state.db.get()?;
        let tx = conn.transaction()?;
        // Photos indexed before whose file or sidecar changed: names of people and pets
        // may have come with it (another app, or an undo).
        let mut again = Vec::new();
        for rec in &recs {
            let id = repo::upsert(&tx, rec)?;
            repo::index_text(&tx, id, rec, &rel)?;
            if existing.contains_key(&rec.name) {
                again.push(id);
            }
        }
        for id in &removed {
            repo::delete_photo(&tx, *id)?;
        }
        repo::refresh_folder(&tx, folder_id)?;
        tx.commit()?;
        state.people.changed(&again);
        {
            let mut p = state.progress.write();
            p.photos_indexed += changed as u64;
        }
        state.emit(Event::Folder { id: folder_id });
        if state.cfg.pregen_thumbs {
            state.images.queue_folder_thumbs(state, folder_id);
        }
    } else {
        // Nothing changed on disk, but keep the pairs in step with the rules (an index
        // written by an older version, or rows moved by a reject).
        let mut conn = state.db.get()?;
        let tx = conn.transaction()?;
        let regrouped = repo::regroup(&tx, folder_id)?;
        tx.execute("UPDATE folders SET scanned_at = ?2 WHERE id = ?1 AND scanned_at = 0", params![folder_id, crate::db::now_ms()])?;
        tx.commit()?;
        if regrouped {
            state.emit(Event::Folder { id: folder_id });
        }
    }
    Ok(FolderScan { changed, removed: removed.len(), total })
}

fn full_scan(state: &AppState, rx: &std::sync::mpsc::Receiver<ScanMsg>) -> Result<()> {
    let started = Instant::now();
    {
        let mut p = state.progress.write();
        p.phase = "walking".into();
        p.current = None;
    }
    state.emit(Event::Progress(state.progress.read().clone()));
    let ok = sync_tree(state)?;
    {
        let mut p = state.progress.write();
        p.library_ok = ok;
        p.message = if ok { None } else { Some("Photo library is not reachable; nothing was removed from the index.".into()) };
    }
    if !ok {
        let mut p = state.progress.write();
        p.phase = "idle".into();
        return Ok(());
    }
    // Newest years first: folder paths sorted descending puts 2026 before 2008.
    let conn = state.db.get()?;
    let mut folders: Vec<(i64, String)> = {
        let mut st = conn.prepare("SELECT id, path FROM folders ORDER BY path DESC")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<std::result::Result<_, _>>()?
    };
    drop(conn);
    // Never-scanned folders first.
    let conn = state.db.get()?;
    let unscanned: HashSet<i64> = {
        let mut st = conn.prepare("SELECT id FROM folders WHERE scanned_at = 0")?;
        let rows = st.query_map([], |r| r.get(0))?;
        rows.collect::<std::result::Result<_, _>>()?
    };
    drop(conn);
    folders.sort_by_key(|(id, _)| !unscanned.contains(id));
    {
        let mut p = state.progress.write();
        p.phase = "indexing".into();
        p.folders_total = folders.len();
        p.folders_done = 0;
    }
    let mut last_emit = Instant::now();
    let (mut photos, mut changed, mut removed) = (0usize, 0usize, 0usize);
    for (i, (id, path)) in folders.iter().enumerate() {
        // Interactive requests jump the queue.
        while let Ok(msg) = rx.try_recv() {
            if let ScanMsg::Folder(fid) = msg {
                let _ = scan_folder(state, fid, false);
            }
        }
        {
            let mut p = state.progress.write();
            p.current = Some(path.clone());
            p.folders_done = i;
        }
        match scan_folder(state, *id, false) {
            Ok(r) => {
                photos += r.total;
                changed += r.changed;
                removed += r.removed;
            }
            Err(e) => tracing::warn!("scan of {path} failed: {e:#}"),
        }
        if last_emit.elapsed() > Duration::from_millis(500) {
            state.emit(Event::Progress(state.progress.read().clone()));
            last_emit = Instant::now();
        }
    }
    {
        let mut p = state.progress.write();
        p.phase = "idle".into();
        p.current = None;
        p.folders_done = folders.len();
        p.last_full_scan = Some(crate::db::now_ms());
    }
    state.emit(Event::Progress(state.progress.read().clone()));
    tracing::info!(
        "full scan: {} folders, {photos} files, {changed} new or changed, {removed} gone from disk ({:.1}s)",
        folders.len(),
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

/// The scanner thread: full scan at start, then periodic re-scans; folder requests
/// from the UI are served in between.
pub fn spawn(state: Arc<AppState>) {
    let (tx, rx) = std::sync::mpsc::channel::<ScanMsg>();
    *state.scan_tx.lock() = Some(tx);
    std::thread::Builder::new()
        .name("scanner".into())
        .spawn(move || {
            let interval = if state.cfg.rescan_minutes == 0 {
                Duration::from_secs(10 * 365 * 86_400) // effectively never
            } else {
                Duration::from_secs(state.cfg.rescan_minutes * 60)
            };
            loop {
                if let Err(e) = full_scan(&state, &rx) {
                    tracing::error!("scan failed: {e:#}");
                    let mut p = state.progress.write();
                    p.phase = "idle".into();
                    p.message = Some(format!("Scan failed: {e}"));
                }
                let deadline = Instant::now() + interval;
                loop {
                    let now = Instant::now();
                    if now >= deadline {
                        break;
                    }
                    match rx.recv_timeout(deadline - now) {
                        Ok(ScanMsg::Folder(id)) => {
                            if let Err(e) = scan_folder(&state, id, false) {
                                tracing::warn!("folder scan failed: {e:#}");
                            }
                        }
                        Ok(ScanMsg::Full) => break,
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => break,
                        Err(_) => return,
                    }
                }
            }
        })
        .expect("spawn scanner");
}

pub fn photo_path(state: &AppState, folder_path: &str, name: &str) -> PathBuf {
    abs_dir(state, folder_path).join(name)
}

