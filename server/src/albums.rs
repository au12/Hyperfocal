//! Albums: hand-picked photos in an order of their own.
//!
//! Like saved collections they live only in the index (`/config` keeps them), and they
//! never touch the photos: deleting an album or taking a photo out of one leaves every
//! file where it is. A member whose file leaves the library (moved or renamed by
//! another app) keeps its place, and is found again when a file with the same name and
//! size turns up. Every change is journaled and can be undone from History.

use crate::api::{ApiError, ApiResult, St, bad};
use crate::state::{AppState, Event};
use anyhow::{Context, Result, anyhow, bail};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::sync::Arc;

/// Photos per change, and per album.
const MAX_BATCH: usize = 20_000;
const MAX_MEMBERS: i64 = 100_000;

pub fn migrate(c: &Connection) -> Result<()> {
    c.execute_batch(
        r#"
        -- (AUTOINCREMENT: an id is never reused, so an undone delete gets its own back.)
        CREATE TABLE IF NOT EXISTS albums(
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            note TEXT,
            cover INTEGER REFERENCES photos(id) ON DELETE SET NULL,
            position INTEGER NOT NULL DEFAULT 0,
            created INTEGER NOT NULL,
            updated INTEGER NOT NULL
        );
        CREATE UNIQUE INDEX IF NOT EXISTS albums_name ON albums(name COLLATE NOCASE);

        -- One row per member, in `pos` order. A member whose photo left the index keeps
        -- its row (photo_id NULL) with the file's name and size to find it again.
        CREATE TABLE IF NOT EXISTS album_photos(
            id INTEGER PRIMARY KEY,
            album_id INTEGER NOT NULL REFERENCES albums(id) ON DELETE CASCADE,
            photo_id INTEGER REFERENCES photos(id) ON DELETE SET NULL,
            pos REAL NOT NULL,
            name TEXT NOT NULL,
            size INTEGER NOT NULL,
            added INTEGER NOT NULL
        );
        CREATE UNIQUE INDEX IF NOT EXISTS album_photos_member ON album_photos(album_id, photo_id) WHERE photo_id IS NOT NULL;
        CREATE INDEX IF NOT EXISTS album_photos_order ON album_photos(album_id, pos);
        CREATE INDEX IF NOT EXISTS album_photos_photo ON album_photos(photo_id) WHERE photo_id IS NOT NULL;
        CREATE INDEX IF NOT EXISTS album_photos_lost ON album_photos(name, size) WHERE photo_id IS NULL;
        "#,
    )?;
    Ok(())
}

/// A photo new to the index: if albums lost a file of this name and size, it is that
/// file, moved. Cheap when nothing is lost (a partial index of the lost members).
pub fn reattach(c: &Connection, photo_id: i64, name: &str, size: i64) -> Result<usize> {
    Ok(c
        .prepare_cached("UPDATE OR IGNORE album_photos SET photo_id = ?1 WHERE photo_id IS NULL AND name = ?2 AND size = ?3")?
        .execute(params![photo_id, name, size])?)
}

/// A photo about to leave the index: if it is in albums and the same file (name and
/// size) is already indexed elsewhere (it was moved, and its new folder was read
/// first), the albums follow it there. Otherwise its rows are kept, lost, by the
/// foreign key (photo_id set to NULL) until the file turns up.
pub fn photo_leaving(c: &Connection, photo_id: i64) -> Result<()> {
    let member = c.prepare_cached("SELECT 1 FROM album_photos WHERE photo_id = ?1 LIMIT 1")?.exists([photo_id])?;
    if !member {
        return Ok(());
    }
    let (name, size): (String, i64) = c.query_row("SELECT name, size FROM photos WHERE id = ?1", [photo_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    let other: Option<i64> = c
        .query_row(
            "SELECT id FROM photos WHERE name = ?1 COLLATE NOCASE AND pair_of IS NULL AND name = ?1 AND size = ?2 AND id != ?3 ORDER BY id DESC LIMIT 1",
            params![name, size, photo_id],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(other) = other {
        c.execute("UPDATE OR IGNORE album_photos SET photo_id = ?2 WHERE photo_id = ?1", params![photo_id, other])?;
    }
    Ok(())
}

// ---------------------------------------------------------------- model

/// A member row, as journaled (enough to put it back exactly).
#[derive(Debug, Clone, PartialEq)]
struct Row {
    photo: Option<i64>,
    pos: f64,
    name: String,
    size: i64,
    added: i64,
}

impl Row {
    fn to_json(&self) -> Value {
        json!([self.photo, self.pos, self.name, self.size, self.added])
    }
    fn from_json(v: &Value) -> Option<Row> {
        let a = v.as_array()?;
        Some(Row {
            photo: a.first()?.as_i64(),
            pos: a.get(1)?.as_f64()?,
            name: a.get(2)?.as_str()?.to_string(),
            size: a.get(3)?.as_i64()?,
            added: a.get(4)?.as_i64()?,
        })
    }
}

fn album_exists(c: &Connection, id: i64) -> Result<bool> {
    Ok(c.query_row("SELECT 1 FROM albums WHERE id = ?1", [id], |_| Ok(())).optional()?.is_some())
}

fn album_name(c: &Connection, id: i64) -> Result<String> {
    c.query_row("SELECT name FROM albums WHERE id = ?1", [id], |r| r.get(0)).optional()?.ok_or_else(|| anyhow!("no such album"))
}

fn touch(c: &Connection, id: i64) -> Result<()> {
    c.execute("UPDATE albums SET updated = ?2 WHERE id = ?1", params![id, crate::db::now_ms()])?;
    Ok(())
}

fn rows_of(c: &Connection, album: i64) -> Result<Vec<Row>> {
    let mut st = c.prepare_cached("SELECT photo_id, pos, name, size, added FROM album_photos WHERE album_id = ?1 ORDER BY pos, id")?;
    let it = st.query_map([album], |r| Ok(Row { photo: r.get(0)?, pos: r.get(1)?, name: r.get(2)?, size: r.get(3)?, added: r.get(4)? }))?;
    Ok(it.collect::<rusqlite::Result<_>>()?)
}

/// Gives every member of an album a whole-number position in its current order. Run
/// when the gaps between neighbours get too small to split. Returns the old positions.
fn renumber(tx: &Transaction, album: i64) -> Result<Vec<(i64, f64)>> {
    let rows: Vec<(i64, f64)> = {
        let mut st = tx.prepare("SELECT id, pos FROM album_photos WHERE album_id = ?1 ORDER BY pos, id")?;
        let it = st.query_map([album], |r| Ok((r.get(0)?, r.get(1)?)))?;
        it.collect::<rusqlite::Result<_>>()?
    };
    let mut up = tx.prepare_cached("UPDATE album_photos SET pos = ?2 WHERE id = ?1")?;
    for (k, (id, _)) in rows.iter().enumerate() {
        up.execute(params![id, (k + 1) as f64])?;
    }
    Ok(rows)
}

/// `n` positions strictly between `lo` and `hi` (`hi` None: after `lo`), or None when
/// the gap is too small for them.
fn spread(lo: f64, hi: Option<f64>, n: usize) -> Option<Vec<f64>> {
    match hi {
        None => Some((1..=n).map(|k| lo + k as f64).collect()),
        Some(hi) => {
            let step = (hi - lo) / (n as f64 + 1.0);
            if !(step > 1e-6) {
                return None;
            }
            Some((1..=n).map(|k| lo + step * k as f64).collect())
        }
    }
}

/// The position just before `before` (a member photo), and that photo's position:
/// the gap new or moved members go into. `before` None: the end of the album.
fn gap_before(tx: &Transaction, album: i64, before: Option<i64>, skip: &HashSet<i64>) -> Result<(f64, Option<f64>)> {
    let rows = rows_of(tx, album)?;
    let kept: Vec<&Row> = rows.iter().filter(|r| r.photo.is_none_or(|p| !skip.contains(&p))).collect();
    let at = match before {
        Some(b) => kept.iter().position(|r| r.photo == Some(b)),
        None => None,
    };
    Ok(match at {
        Some(k) => (if k == 0 { kept[0].pos - 1.0 } else { kept[k - 1].pos }, Some(kept[k].pos)),
        None => (kept.last().map(|r| r.pos).unwrap_or(0.0), None),
    })
}

/// The photos of `ids` that exist and are shown in lists (a JPEG folded into its RAW is
/// added as the RAW), in the order given, without repeats.
fn resolve(c: &Connection, ids: &[i64]) -> Result<Vec<(i64, String, i64)>> {
    let mut st = c.prepare_cached("SELECT COALESCE(p.pair_of, p.id) FROM photos p WHERE p.id = ?1")?;
    let mut info = c.prepare_cached("SELECT name, size FROM photos WHERE id = ?1")?;
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(ids.len());
    for &id in ids {
        let Some(main) = st.query_row([id], |r| r.get::<_, i64>(0)).optional()? else { continue };
        if !seen.insert(main) {
            continue;
        }
        if let Some((name, size)) = info.query_row([main], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))).optional()? {
            out.push((main, name, size));
        }
    }
    Ok(out)
}

/// Adds photos (those not in the album yet) before `before` or at the end. Returns the
/// photos added.
fn add(tx: &Transaction, album: i64, ids: &[i64], before: Option<i64>) -> Result<Vec<i64>> {
    let present: HashSet<i64> = {
        let mut st = tx.prepare("SELECT photo_id FROM album_photos WHERE album_id = ?1 AND photo_id IS NOT NULL")?;
        let it = st.query_map([album], |r| r.get(0))?;
        it.collect::<rusqlite::Result<_>>()?
    };
    let todo: Vec<(i64, String, i64)> = resolve(tx, ids)?.into_iter().filter(|(id, _, _)| !present.contains(id)).collect();
    if todo.is_empty() {
        return Ok(Vec::new());
    }
    let count: i64 = tx.query_row("SELECT count(*) FROM album_photos WHERE album_id = ?1", [album], |r| r.get(0))?;
    if count + todo.len() as i64 > MAX_MEMBERS {
        bail!("an album holds at most {MAX_MEMBERS} photos");
    }
    let before = before.filter(|b| present.contains(b));
    let (lo, hi) = gap_before(tx, album, before, &HashSet::new())?;
    let pos = match spread(lo, hi, todo.len()) {
        Some(p) => p,
        None => {
            renumber(tx, album)?;
            let (lo, hi) = gap_before(tx, album, before, &HashSet::new())?;
            spread(lo, hi, todo.len()).context("no room in the album's order")?
        }
    };
    let now = crate::db::now_ms();
    let mut ins = tx.prepare_cached("INSERT INTO album_photos(album_id, photo_id, pos, name, size, added) VALUES(?1, ?2, ?3, ?4, ?5, ?6)")?;
    for ((id, name, size), p) in todo.iter().zip(pos) {
        ins.execute(params![album, id, p, name, size, now])?;
    }
    touch(tx, album)?;
    Ok(todo.into_iter().map(|t| t.0).collect())
}

/// Takes photos out of the album. Returns the rows removed.
fn remove(tx: &Transaction, album: i64, ids: &[i64]) -> Result<Vec<Row>> {
    let mut get = tx.prepare_cached("SELECT photo_id, pos, name, size, added FROM album_photos WHERE album_id = ?1 AND photo_id = ?2")?;
    let mut del = tx.prepare_cached("DELETE FROM album_photos WHERE album_id = ?1 AND photo_id = ?2")?;
    let mut out = Vec::new();
    for &id in ids {
        if let Some(r) = get
            .query_row(params![album, id], |r| Ok(Row { photo: r.get(0)?, pos: r.get(1)?, name: r.get(2)?, size: r.get(3)?, added: r.get(4)? }))
            .optional()?
        {
            del.execute(params![album, id])?;
            out.push(r);
        }
    }
    if !out.is_empty() {
        touch(tx, album)?;
    }
    Ok(out)
}

/// Moves members (in their current order) before `before`, or to the end. Returns the
/// old positions of what moved, by photo (and of every member, when the album had to be
/// renumbered: `renumbered` true).
fn reorder(tx: &Transaction, album: i64, ids: &[i64], before: Option<i64>) -> Result<(Vec<(i64, f64)>, bool)> {
    let rows = rows_of(tx, album)?;
    let want: HashSet<i64> = ids.iter().copied().collect();
    let moving: Vec<&Row> = rows.iter().filter(|r| r.photo.is_some_and(|p| want.contains(&p))).collect();
    if moving.is_empty() || before.is_some_and(|b| want.contains(&b)) {
        return Ok((Vec::new(), false));
    }
    let moving_ids: HashSet<i64> = moving.iter().filter_map(|r| r.photo).collect();
    let (lo, hi) = gap_before(tx, album, before, &moving_ids)?;
    let mut old: Vec<(i64, f64)> = moving.iter().map(|r| (r.photo.unwrap(), r.pos)).collect();
    let mut renumbered = false;
    let pos = match spread(lo, hi, moving.len()) {
        Some(p) => p,
        None => {
            // Every member's old position goes in the journal: renumbering moves them all.
            let before_all = rows_of(tx, album)?;
            old = before_all.iter().filter_map(|r| r.photo.map(|p| (p, r.pos))).collect();
            renumbered = true;
            renumber(tx, album)?;
            let (lo, hi) = gap_before(tx, album, before, &moving_ids)?;
            spread(lo, hi, moving.len()).context("no room in the album's order")?
        }
    };
    let mut up = tx.prepare_cached("UPDATE album_photos SET pos = ?3 WHERE album_id = ?1 AND photo_id = ?2")?;
    for (r, p) in moving.iter().zip(pos) {
        up.execute(params![album, r.photo, p])?;
    }
    touch(tx, album)?;
    Ok((old, renumbered))
}

/// Puts rows back (an undone removal): rows whose photo is in the album again are
/// skipped; lost rows come back as lost.
fn put_back(tx: &Transaction, album: i64, rows: &[Row]) -> Result<Vec<i64>> {
    let mut ins = tx.prepare_cached("INSERT OR IGNORE INTO album_photos(album_id, photo_id, pos, name, size, added) VALUES(?1, ?2, ?3, ?4, ?5, ?6)")?;
    let mut exists = tx.prepare_cached("SELECT 1 FROM photos WHERE id = ?1")?;
    let mut back = Vec::new();
    for r in rows {
        // A photo that left the index since: its row comes back lost (and may be found).
        let photo = r.photo.filter(|p| exists.exists([p]).unwrap_or(false));
        if ins.execute(params![album, photo, r.pos, r.name, r.size, r.added])? > 0 {
            back.extend(photo);
        }
    }
    touch(tx, album)?;
    Ok(back)
}

/// Positions back to what they were (an undone reorder).
fn set_positions(tx: &Transaction, album: i64, pos: &[(i64, f64)]) -> Result<Vec<(i64, f64)>> {
    let mut get = tx.prepare_cached("SELECT pos FROM album_photos WHERE album_id = ?1 AND photo_id = ?2")?;
    let mut up = tx.prepare_cached("UPDATE album_photos SET pos = ?3 WHERE album_id = ?1 AND photo_id = ?2")?;
    let mut now = Vec::new();
    for &(p, at) in pos {
        if let Some(cur) = get.query_row(params![album, p], |r| r.get::<_, f64>(0)).optional()? {
            up.execute(params![album, p, at])?;
            now.push((p, cur));
        }
    }
    touch(tx, album)?;
    Ok(now)
}

fn album_json(c: &Connection, id: i64) -> Result<Value> {
    c.query_row("SELECT id, name, note, cover, position, created, updated FROM albums WHERE id = ?1", [id], |r| {
        Ok(json!({
            "id": r.get::<_, i64>(0)?, "name": r.get::<_, String>(1)?, "note": r.get::<_, Option<String>>(2)?,
            "cover": r.get::<_, Option<i64>>(3)?, "position": r.get::<_, i64>(4)?,
            "created": r.get::<_, i64>(5)?, "updated": r.get::<_, i64>(6)?,
        }))
    })
    .optional()?
    .ok_or_else(|| anyhow!("no such album"))
}

/// Deletes an album (never its photos). Returns what is needed to bring it back.
fn delete(tx: &Transaction, id: i64) -> Result<Value> {
    let album = album_json(tx, id)?;
    let rows: Vec<Value> = rows_of(tx, id)?.iter().map(Row::to_json).collect();
    tx.execute("DELETE FROM albums WHERE id = ?1", [id])?;
    Ok(json!({ "album": album, "rows": rows }))
}

/// Brings a deleted album back, under its own id (and a fresh name if one was taken).
fn undelete(tx: &Transaction, v: &Value) -> Result<i64> {
    let a = v.get("album").context("bad journal entry")?;
    let id = a["id"].as_i64().context("bad journal entry")?;
    let mut name = a["name"].as_str().unwrap_or("Album").to_string();
    while tx.query_row("SELECT 1 FROM albums WHERE name = ?1 COLLATE NOCASE", [&name], |_| Ok(())).optional()?.is_some() {
        name = format!("{name} (restored)");
    }
    let cover = a["cover"].as_i64().filter(|p| tx.query_row("SELECT 1 FROM photos WHERE id = ?1", [p], |_| Ok(())).optional().ok().flatten().is_some());
    tx.execute(
        "INSERT INTO albums(id, name, note, cover, position, created, updated) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![id, name, a["note"].as_str(), cover, a["position"].as_i64().unwrap_or(0), a["created"].as_i64().unwrap_or(0), crate::db::now_ms()],
    )?;
    let rows: Vec<Row> = v["rows"].as_array().map(|a| a.iter().filter_map(Row::from_json).collect()).unwrap_or_default();
    put_back(tx, id, &rows)?;
    Ok(id)
}

fn edit(tx: &Transaction, id: i64, name: Option<&str>, note: Option<Option<&str>>, cover: Option<Option<i64>>) -> Result<Value> {
    let before = album_json(tx, id)?;
    if let Some(n) = name {
        let n = clean_name(n)?;
        let clash: Option<i64> = tx.query_row("SELECT id FROM albums WHERE name = ?1 COLLATE NOCASE AND id != ?2", params![n, id], |r| r.get(0)).optional()?;
        if clash.is_some() {
            bail!("there is already an album called “{n}”");
        }
        tx.execute("UPDATE albums SET name = ?2 WHERE id = ?1", params![id, n])?;
    }
    if let Some(n) = note {
        let n = n.map(str::trim).filter(|s| !s.is_empty());
        if n.is_some_and(|s| s.chars().count() > 2000) {
            bail!("a note of at most 2000 characters");
        }
        tx.execute("UPDATE albums SET note = ?2 WHERE id = ?1", params![id, n])?;
    }
    if let Some(cv) = cover {
        if let Some(p) = cv {
            let member = tx.query_row("SELECT 1 FROM album_photos WHERE album_id = ?1 AND photo_id = ?2", params![id, p], |_| Ok(())).optional()?.is_some();
            if !member {
                bail!("the cover must be a photo of the album");
            }
        }
        tx.execute("UPDATE albums SET cover = ?2 WHERE id = ?1", params![id, cv])?;
    }
    touch(tx, id)?;
    Ok(json!({ "name": before["name"], "note": before["note"], "cover": before["cover"] }))
}

fn clean_name(n: &str) -> Result<String> {
    let n = n.trim();
    if n.is_empty() || n.chars().count() > 120 {
        bail!("an album needs a name of at most 120 characters");
    }
    Ok(n.to_string())
}

fn create(tx: &Transaction, name: &str, note: Option<&str>) -> Result<i64> {
    let name = clean_name(name)?;
    if tx.query_row("SELECT 1 FROM albums WHERE name = ?1 COLLATE NOCASE", [&name], |_| Ok(())).optional()?.is_some() {
        bail!("there is already an album called “{name}”");
    }
    let now = crate::db::now_ms();
    let position: i64 = tx.query_row("SELECT COALESCE(max(position), 0) + 1 FROM albums", [], |r| r.get(0))?;
    Ok(tx.query_row(
        "INSERT INTO albums(name, note, position, created, updated) VALUES(?1, ?2, ?3, ?4, ?4) RETURNING id",
        params![name, note.map(str::trim).filter(|s| !s.is_empty()), position, now],
        |r| r.get(0),
    )?)
}

/// Every album with its counts (photos shown, members whose file is gone) and cover.
pub fn list(c: &Connection) -> Result<Vec<Value>> {
    let mut st = c.prepare(
        "SELECT a.id, a.name, a.note, a.position, a.created, a.updated,
                (SELECT count(*) FROM album_photos ap JOIN photos p ON p.id = ap.photo_id JOIN folders f ON f.id = p.folder_id
                  WHERE ap.album_id = a.id AND f.is_reject = 0),
                (SELECT count(*) FROM album_photos ap WHERE ap.album_id = a.id AND ap.photo_id IS NULL),
                COALESCE(
                  (SELECT ap.photo_id FROM album_photos ap WHERE ap.album_id = a.id AND ap.photo_id = a.cover),
                  (SELECT ap.photo_id FROM album_photos ap JOIN photos p ON p.id = ap.photo_id JOIN folders f ON f.id = p.folder_id
                    WHERE ap.album_id = a.id AND f.is_reject = 0 ORDER BY ap.pos LIMIT 1)),
                a.cover
         FROM albums a ORDER BY a.position, a.name COLLATE NOCASE",
    )?;
    let rows: Vec<(Value, Option<i64>)> = st
        .query_map([], |r| {
            Ok((
                json!({
                    "id": r.get::<_, i64>(0)?, "name": r.get::<_, String>(1)?, "note": r.get::<_, Option<String>>(2)?,
                    "position": r.get::<_, i64>(3)?, "created": r.get::<_, i64>(4)?, "updated": r.get::<_, i64>(5)?,
                    "count": r.get::<_, i64>(6)?, "missing": r.get::<_, i64>(7)?, "cover_set": r.get::<_, Option<i64>>(9)?,
                }),
                r.get::<_, Option<i64>>(8)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut sig = c.prepare_cached("SELECT size, mtime, orientation FROM photos WHERE id = ?1")?;
    let mut out = Vec::with_capacity(rows.len());
    for (mut v, cover) in rows {
        v["cover"] = match cover {
            Some(id) => match sig.query_row([id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i32>(2)?))).optional()? {
                Some((size, mtime, orientation)) => {
                    let rec = crate::model::PhotoRec { id, size, mtime, orientation, ..Default::default() };
                    json!({ "id": id, "v": crate::images::sig_str(&rec) })
                }
                None => Value::Null,
            },
            None => Value::Null,
        };
        out.push(v);
    }
    Ok(out)
}

/// The albums a photo is in: `[[id, name], ...]`.
pub fn of_photo(c: &Connection, photo: i64) -> Result<Vec<Value>> {
    let mut st = c.prepare_cached(
        "SELECT a.id, a.name FROM album_photos ap JOIN albums a ON a.id = ap.album_id WHERE ap.photo_id = ?1 ORDER BY a.position, a.name COLLATE NOCASE",
    )?;
    let it = st.query_map([photo], |r| Ok(json!([r.get::<_, i64>(0)?, r.get::<_, String>(1)?])))?;
    Ok(it.collect::<rusqlite::Result<_>>()?)
}

// ---------------------------------------------------------------- journal and undo

fn plural(n: usize) -> String {
    if n == 1 { "1 photo".into() } else { format!("{n} photos") }
}

fn journal(tx: &Transaction, batch: &str, summary: &str, data: Value) -> Result<()> {
    tx.execute(
        "INSERT INTO journal(batch, ts, kind, photo_id, summary, data) VALUES(?1, ?2, 'album', NULL, ?3, ?4)",
        params![batch, crate::db::now_ms(), summary, data.to_string()],
    )?;
    Ok(())
}

/// Whether every change of a batch is an album change (those undo in read-only mode
/// too: they only touch the index).
pub fn batch_is_albums(c: &Connection, batch: &str) -> Result<bool> {
    let (all, albums): (i64, i64) = c.query_row(
        "SELECT count(*), coalesce(sum(kind = 'album'), 0) FROM journal WHERE batch = ?1 AND undone = 0",
        [batch],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    Ok(all > 0 && all == albums)
}

/// Undoes one journaled album change and journals its inverse in `new_batch`, so the
/// undo can be undone in turn. Returns the photos it touched.
pub fn undo_entry(state: &AppState, jid: i64, v: &Value, new_batch: &str, of_batch: &str) -> Result<Vec<i64>> {
    let mut c = state.db.get()?;
    let tx = c.transaction()?;
    let album = v["album"].as_i64().or_else(|| v["album"]["id"].as_i64()).context("bad journal entry")?;
    let op = v["op"].as_str().unwrap_or("");
    let mut touched = Vec::new();
    if op != "delete" && op != "create" && !album_exists(&tx, album)? {
        bail!("the album is gone");
    }
    let inverse = match op {
        "create" => {
            let gone = delete(&tx, album)?;
            json!({ "op": "delete", "album": gone["album"], "rows": gone["rows"] })
        }
        "delete" => {
            let id = undelete(&tx, v)?;
            json!({ "op": "create", "album": id })
        }
        "add" => {
            let ids: Vec<i64> = v["photos"].as_array().map(|a| a.iter().filter_map(Value::as_i64).collect()).unwrap_or_default();
            let rows = remove(&tx, album, &ids)?;
            touched.extend(rows.iter().filter_map(|r| r.photo));
            json!({ "op": "remove", "album": album, "rows": rows.iter().map(Row::to_json).collect::<Vec<_>>() })
        }
        "remove" => {
            let rows: Vec<Row> = v["rows"].as_array().map(|a| a.iter().filter_map(Row::from_json).collect()).unwrap_or_default();
            let back = put_back(&tx, album, &rows)?;
            touched.extend(&back);
            json!({ "op": "add", "album": album, "photos": back })
        }
        "order" => {
            let pos: Vec<(i64, f64)> =
                v["pos"].as_array().map(|a| a.iter().filter_map(|p| Some((p.get(0)?.as_i64()?, p.get(1)?.as_f64()?))).collect()).unwrap_or_default();
            let now = set_positions(&tx, album, &pos)?;
            touched.extend(now.iter().map(|p| p.0));
            json!({ "op": "order", "album": album, "pos": now })
        }
        "edit" => {
            let b = &v["before"];
            let before = edit(
                &tx,
                album,
                b["name"].as_str(),
                Some(b["note"].as_str()),
                Some(b["cover"].as_i64().filter(|p| {
                    tx.query_row("SELECT 1 FROM album_photos WHERE album_id = ?1 AND photo_id = ?2", params![album, p], |_| Ok(())).optional().ok().flatten().is_some()
                })),
            )?;
            json!({ "op": "edit", "album": album, "before": before })
        }
        _ => bail!("bad journal entry"),
    };
    let summary = format!("Undo: {}", tx.query_row("SELECT summary FROM journal WHERE id = ?1", [jid], |r| r.get::<_, Option<String>>(0))?.unwrap_or_default());
    let mut inverse = inverse;
    inverse["undo_of"] = json!(of_batch);
    journal(&tx, new_batch, &summary, inverse)?;
    tx.execute("UPDATE journal SET undone = 1 WHERE id = ?1", [jid])?;
    tx.commit()?;
    state.emit(Event::Albums);
    Ok(touched)
}

// ---------------------------------------------------------------- HTTP

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/albums", get(get_albums).post(create_album))
        .route("/albums/{id}", post(edit_album).delete(delete_album))
        .route("/albums/{id}/add", post(add_photos))
        .route("/albums/{id}/remove", post(remove_photos))
        .route("/albums/{id}/order", post(order_photos))
        .route("/albums/{id}/forget", post(forget_missing))
        .route("/albums/order", post(order_albums))
}

fn conflict(e: anyhow::Error) -> ApiError {
    let msg = format!("{e:#}");
    if msg.contains("no such album") {
        ApiError(StatusCode::NOT_FOUND, msg)
    } else {
        bad(msg)
    }
}

/// Runs an album change in a transaction off the async runtime; tells every client.
async fn change<T: Send + 'static>(state: &Arc<AppState>, f: impl FnOnce(&Transaction) -> Result<T> + Send + 'static) -> ApiResult<T> {
    let st = state.clone();
    let out = tokio::task::spawn_blocking(move || -> Result<T> {
        let mut c = st.db.get()?;
        let tx = c.transaction()?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    })
    .await
    .map_err(|e| anyhow!("{e}"))?
    .map_err(conflict)?;
    state.emit(Event::Albums);
    Ok(out)
}

async fn get_albums(State(state): St) -> ApiResult<Json<Value>> {
    let st = state.clone();
    let v = tokio::task::spawn_blocking(move || -> Result<Value> { Ok(json!(list(&*st.db.get()?)?)) }).await.map_err(|e| anyhow!("{e}"))??;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct CreateBody {
    name: String,
    note: Option<String>,
    /// Photos to start with.
    #[serde(default)]
    ids: Vec<i64>,
}

async fn create_album(State(state): St, Json(b): Json<CreateBody>) -> ApiResult<Json<Value>> {
    if b.ids.len() > MAX_BATCH {
        return Err(bad(format!("at most {MAX_BATCH} photos per change")));
    }
    let batch = crate::ops::new_batch();
    let bt = batch.clone();
    let (id, added) = change(&state, move |tx| {
        let id = create(tx, &b.name, b.note.as_deref())?;
        let added = add(tx, id, &b.ids, None)?;
        let name = album_name(tx, id)?;
        let what = if added.is_empty() { String::new() } else { format!(" with {}", plural(added.len())) };
        journal(tx, &bt, &format!("Created album “{name}”{what}"), json!({ "op": "create", "album": id }))?;
        Ok((id, added))
    })
    .await?;
    let c = state.db.get()?;
    let mut a = album_json(&c, id)?;
    a["added"] = json!(added);
    a["batch"] = json!(batch);
    Ok(Json(a))
}

#[derive(Deserialize)]
struct EditBody {
    name: Option<String>,
    /// null clears it.
    #[serde(default, deserialize_with = "crate::ops::double_option")]
    note: Option<Option<String>>,
    /// A member photo, or null for the first one.
    #[serde(default, deserialize_with = "crate::ops::double_option")]
    cover: Option<Option<i64>>,
}

async fn edit_album(State(state): St, Path(id): Path<i64>, Json(b): Json<EditBody>) -> ApiResult<Json<Value>> {
    let batch = crate::ops::new_batch();
    let bt = batch.clone();
    change(&state, move |tx| {
        let old = album_name(tx, id)?;
        let before = edit(tx, id, b.name.as_deref(), b.note.as_ref().map(|n| n.as_deref()), b.cover)?;
        let summary = match (&b.name, &b.cover) {
            (Some(n), _) if n.trim() != old => format!("Renamed album “{old}” to “{}”", n.trim()),
            (_, Some(_)) => format!("Changed the cover of “{old}”"),
            _ => format!("Edited album “{old}”"),
        };
        journal(tx, &bt, &summary, json!({ "op": "edit", "album": id, "before": before }))
    })
    .await?;
    let c = state.db.get()?;
    let mut a = album_json(&c, id)?;
    a["batch"] = json!(batch);
    Ok(Json(a))
}

async fn delete_album(State(state): St, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let batch = crate::ops::new_batch();
    let bt = batch.clone();
    change(&state, move |tx| {
        let name = album_name(tx, id)?;
        let mut gone = delete(tx, id)?;
        gone["op"] = json!("delete");
        journal(tx, &bt, &format!("Deleted album “{name}” (its photos stay)"), gone)
    })
    .await?;
    Ok(Json(json!({ "ok": true, "batch": batch })))
}

#[derive(Deserialize)]
struct PhotosBody {
    ids: Vec<i64>,
    /// Put them before this member photo (default: at the end).
    before: Option<i64>,
}

fn check_ids(ids: &[i64]) -> ApiResult<()> {
    if ids.is_empty() || ids.len() > MAX_BATCH {
        return Err(bad(format!("between 1 and {MAX_BATCH} photos per change")));
    }
    Ok(())
}

async fn add_photos(State(state): St, Path(id): Path<i64>, Json(b): Json<PhotosBody>) -> ApiResult<Json<Value>> {
    check_ids(&b.ids)?;
    let batch = crate::ops::new_batch();
    let bt = batch.clone();
    let added = change(&state, move |tx| {
        let name = album_name(tx, id)?;
        let added = add(tx, id, &b.ids, b.before)?;
        if !added.is_empty() {
            journal(tx, &bt, &format!("Added {} to “{name}”", plural(added.len())), json!({ "op": "add", "album": id, "photos": added }))?;
        }
        Ok(added)
    })
    .await?;
    Ok(Json(json!({ "added": added, "batch": if added.is_empty() { Value::Null } else { json!(batch) } })))
}

async fn remove_photos(State(state): St, Path(id): Path<i64>, Json(b): Json<PhotosBody>) -> ApiResult<Json<Value>> {
    check_ids(&b.ids)?;
    let batch = crate::ops::new_batch();
    let bt = batch.clone();
    let removed = change(&state, move |tx| {
        let name = album_name(tx, id)?;
        let rows = remove(tx, id, &b.ids)?;
        if !rows.is_empty() {
            let data = json!({ "op": "remove", "album": id, "rows": rows.iter().map(Row::to_json).collect::<Vec<_>>() });
            journal(tx, &bt, &format!("Took {} out of “{name}” (the files stay)", plural(rows.len())), data)?;
        }
        Ok(rows.into_iter().filter_map(|r| r.photo).collect::<Vec<_>>())
    })
    .await?;
    Ok(Json(json!({ "removed": removed, "batch": if removed.is_empty() { Value::Null } else { json!(batch) } })))
}

async fn order_photos(State(state): St, Path(id): Path<i64>, Json(b): Json<PhotosBody>) -> ApiResult<Json<Value>> {
    check_ids(&b.ids)?;
    let batch = crate::ops::new_batch();
    let bt = batch.clone();
    let moved = change(&state, move |tx| {
        let name = album_name(tx, id)?;
        let (old, _) = reorder(tx, id, &b.ids, b.before)?;
        if !old.is_empty() {
            journal(tx, &bt, &format!("Reordered “{name}”"), json!({ "op": "order", "album": id, "pos": old }))?;
        }
        Ok(!old.is_empty())
    })
    .await?;
    Ok(Json(json!({ "moved": moved, "batch": if moved { json!(batch) } else { Value::Null } })))
}

async fn forget_missing(State(state): St, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let batch = crate::ops::new_batch();
    let bt = batch.clone();
    let n = change(&state, move |tx| {
        let name = album_name(tx, id)?;
        let rows: Vec<Row> = rows_of(tx, id)?.into_iter().filter(|r| r.photo.is_none()).collect();
        tx.execute("DELETE FROM album_photos WHERE album_id = ?1 AND photo_id IS NULL", [id])?;
        if !rows.is_empty() {
            let data = json!({ "op": "remove", "album": id, "rows": rows.iter().map(Row::to_json).collect::<Vec<_>>() });
            journal(tx, &bt, &format!("Forgot {} missing from “{name}”", plural(rows.len())), data)?;
            touch(tx, id)?;
        }
        Ok(rows.len())
    })
    .await?;
    Ok(Json(json!({ "forgotten": n, "batch": if n == 0 { Value::Null } else { json!(batch) } })))
}

#[derive(Deserialize)]
struct AlbumOrderBody {
    ids: Vec<i64>,
}

/// The order of the albums themselves (the sidebar): ids first to last.
async fn order_albums(State(state): St, Json(b): Json<AlbumOrderBody>) -> ApiResult<Json<Value>> {
    if b.ids.len() > 10_000 {
        return Err(bad("too many albums"));
    }
    change(&state, move |tx| {
        let mut up = tx.prepare_cached("UPDATE albums SET position = ?2 WHERE id = ?1")?;
        for (k, id) in b.ids.iter().enumerate() {
            up.execute(params![id, k as i64 + 1])?;
        }
        Ok(())
    })
    .await?;
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, crate::db::Pool) {
        let tmp = tempfile::tempdir().unwrap();
        let pool = crate::db::open(&tmp.path().join("index.db")).unwrap();
        {
            let c = pool.get().unwrap();
            c.execute("INSERT INTO folders(id, parent_id, path, name, scanned_at) VALUES(1, NULL, '', 'Library', 1)", []).unwrap();
            for k in 1..=6 {
                let name = format!("IMG_{k}.CR3");
                let rec = crate::model::PhotoRec { folder_id: 1, name: name.clone(), stem: name.clone(), ext: "cr3".into(), kind: 1, size: 1000 + k, ..Default::default() };
                crate::repo::upsert(&c, &rec).unwrap();
            }
        }
        (tmp, pool)
    }

    fn order(c: &Connection, album: i64) -> Vec<Option<i64>> {
        rows_of(c, album).unwrap().into_iter().map(|r| r.photo).collect()
    }

    #[test]
    fn add_remove_and_reorder_keep_an_order_of_their_own() {
        let (_t, pool) = setup();
        let mut c = pool.get().unwrap();
        let tx = c.transaction().unwrap();
        let a = create(&tx, " Trip ", None).unwrap();
        assert!(create(&tx, "trip", None).is_err(), "names are unique, whatever the case");
        assert_eq!(add(&tx, a, &[3, 1, 3, 2, 99], None).unwrap(), vec![3, 1, 2], "no repeats, unknown photos skipped");
        assert_eq!(add(&tx, a, &[1, 4], Some(1)).unwrap(), vec![4], "already there: skipped; the new one goes before 1");
        assert_eq!(order(&tx, a), vec![Some(3), Some(4), Some(1), Some(2)]);
        let (old, _) = reorder(&tx, a, &[2, 3], Some(4)).unwrap();
        assert_eq!(order(&tx, a), vec![Some(3), Some(2), Some(4), Some(1)], "moved ones keep their own order");
        assert_eq!(old.len(), 2);
        reorder(&tx, a, &[3], None).unwrap();
        assert_eq!(order(&tx, a), vec![Some(2), Some(4), Some(1), Some(3)]);
        // Undoing the reorder puts the positions back.
        set_positions(&tx, a, &old).unwrap();
        let removed = remove(&tx, a, &[4, 6]).unwrap();
        assert_eq!(removed.len(), 1);
        assert!(!order(&tx, a).contains(&Some(4)));
        put_back(&tx, a, &removed).unwrap();
        assert!(order(&tx, a).contains(&Some(4)));
        tx.commit().unwrap();
    }

    #[test]
    fn many_moves_into_one_gap_renumber_instead_of_running_out() {
        let (_t, pool) = setup();
        let mut c = pool.get().unwrap();
        let tx = c.transaction().unwrap();
        let a = create(&tx, "Gaps", None).unwrap();
        add(&tx, a, &[1, 2, 3], None).unwrap();
        // Halving the gap before 2 sixty times exhausts it; renumbering must step in.
        for _ in 0..60 {
            reorder(&tx, a, &[3], Some(2)).unwrap();
            reorder(&tx, a, &[1], Some(3)).unwrap();
        }
        let o = order(&tx, a);
        assert_eq!(o.len(), 3);
        let pos: Vec<f64> = rows_of(&tx, a).unwrap().iter().map(|r| r.pos).collect();
        assert!(pos.windows(2).all(|w| w[0] < w[1]), "{pos:?}");
        tx.commit().unwrap();
    }

    #[test]
    fn a_member_whose_file_moves_away_and_back_keeps_its_place() {
        let (_t, pool) = setup();
        let mut c = pool.get().unwrap();
        let a = {
            let tx = c.transaction().unwrap();
            let a = create(&tx, "Keep", None).unwrap();
            add(&tx, a, &[1, 2, 3], None).unwrap();
            tx.commit().unwrap();
            a
        };
        // The file of photo 2 is moved outside the app: its index row goes...
        crate::repo::delete_photo(&c, 2).unwrap();
        assert_eq!(order(&c, a), vec![Some(1), None, Some(3)]);
        let l = list(&c).unwrap();
        assert_eq!((l[0]["count"].as_i64(), l[0]["missing"].as_i64()), (Some(2), Some(1)));
        // ...and turns up in another folder under a new id.
        c.execute("INSERT INTO folders(id, parent_id, path, name, scanned_at) VALUES(2, 1, 'Moved', 'Moved', 1)", []).unwrap();
        let rec = crate::model::PhotoRec { folder_id: 2, name: "IMG_2.CR3".into(), stem: "IMG_2".into(), ext: "cr3".into(), kind: 1, size: 1002, ..Default::default() };
        let id = crate::repo::upsert(&c, &rec).unwrap();
        assert_eq!(reattach(&c, id, "IMG_2.CR3", 1002).unwrap(), 1);
        assert_eq!(order(&c, a), vec![Some(1), Some(id), Some(3)], "found again, in its old place");
        // A file of the same name but another size is someone else.
        crate::repo::delete_photo(&c, 3).unwrap();
        assert_eq!(reattach(&c, 77, "IMG_3.CR3", 5).unwrap(), 0);
    }

    #[test]
    fn a_member_moved_into_a_folder_read_first_is_followed_there() {
        let (_t, pool) = setup();
        let mut c = pool.get().unwrap();
        let a = {
            let tx = c.transaction().unwrap();
            let a = create(&tx, "Follow", None).unwrap();
            add(&tx, a, &[3, 4], None).unwrap();
            tx.commit().unwrap();
            a
        };
        // The new folder is read before the old one: the moved file is indexed (and
        // finds nothing lost) while its old row is still there...
        c.execute("INSERT INTO folders(id, parent_id, path, name, scanned_at) VALUES(2, 1, 'New', 'New', 1)", []).unwrap();
        let rec = crate::model::PhotoRec { folder_id: 2, name: "IMG_3.CR3".into(), stem: "IMG_3".into(), ext: "cr3".into(), kind: 1, size: 1003, ..Default::default() };
        let id = crate::repo::upsert(&c, &rec).unwrap();
        assert_eq!(reattach(&c, id, "IMG_3.CR3", 1003).unwrap(), 0);
        // ...then the old row goes, and the album follows the file.
        crate::repo::delete_photo(&c, 3).unwrap();
        assert_eq!(order(&c, a), vec![Some(id), Some(4)]);
    }

    #[test]
    fn deleting_an_album_never_touches_photos_and_comes_back_whole() {
        let (_t, pool) = setup();
        let mut c = pool.get().unwrap();
        let tx = c.transaction().unwrap();
        let a = create(&tx, "Gone", Some("note")).unwrap();
        add(&tx, a, &[5, 4], None).unwrap();
        edit(&tx, a, None, None, Some(Some(4))).unwrap();
        let mut gone = delete(&tx, a).unwrap();
        gone["op"] = json!("delete");
        let photos: i64 = tx.query_row("SELECT count(*) FROM photos", [], |r| r.get(0)).unwrap();
        assert_eq!(photos, 6);
        assert!(!album_exists(&tx, a).unwrap());
        let back = undelete(&tx, &gone).unwrap();
        assert_eq!(back, a, "the same id");
        assert_eq!(order(&tx, a), vec![Some(5), Some(4)]);
        let j = album_json(&tx, a).unwrap();
        assert_eq!((j["name"].as_str(), j["note"].as_str(), j["cover"].as_i64()), (Some("Gone"), Some("note"), Some(4)));
        assert!(edit(&tx, a, None, None, Some(Some(1))).is_err(), "the cover is a member");
        tx.commit().unwrap();
    }

    #[test]
    fn a_folded_jpeg_is_added_as_its_raw() {
        let (_t, pool) = setup();
        let mut c = pool.get().unwrap();
        c.execute("UPDATE photos SET pair_of = 1 WHERE id = 6", []).unwrap();
        let tx = c.transaction().unwrap();
        let a = create(&tx, "Pairs", None).unwrap();
        assert_eq!(add(&tx, a, &[6, 1], None).unwrap(), vec![1]);
        tx.commit().unwrap();
    }

    #[tokio::test]
    async fn the_http_api_journals_every_change_and_undoes_it() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("photos")).unwrap();
        let mut cfg = crate::config::Config::for_tests(tmp.path());
        cfg.readonly = true;
        let state = crate::build_state(cfg).await.unwrap();
        {
            let c = state.db.get().unwrap();
            c.execute("INSERT INTO folders(id, parent_id, path, name, scanned_at) VALUES(1, NULL, '', 'Library', 1)", []).unwrap();
            for k in 1..=4 {
                let name = format!("P{k}.CR3");
                let rec = crate::model::PhotoRec { folder_id: 1, name: name.clone(), stem: name, ext: "cr3".into(), kind: 1, taken: Some(1_600_000_000 + k), ..Default::default() };
                crate::repo::upsert(&c, &rec).unwrap();
            }
        }
        let app = crate::api::router(state.clone());
        let call = |method: &'static str, uri: String, body: Option<Value>| {
            let app = app.clone();
            async move {
                let req = Request::builder().method(method).uri(uri).header("content-type", "application/json");
                let resp = app.oneshot(req.body(Body::from(body.map(|b| b.to_string()).unwrap_or_default())).unwrap()).await.unwrap();
                let st = resp.status();
                let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
                (st, serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null))
            }
        };
        // Works in read-only mode: albums are only in the index.
        let (st, a) = call("POST", "/api/albums".into(), Some(json!({"name": "Best", "ids": [2, 1]}))).await;
        assert_eq!(st, StatusCode::OK, "{a}");
        let id = a["id"].as_i64().unwrap();
        assert_eq!(a["added"], json!([2, 1]));
        assert_eq!(call("POST", "/api/albums".into(), Some(json!({"name": "best"}))).await.0, StatusCode::BAD_REQUEST);
        let (_, r) = call("POST", format!("/api/albums/{id}/add"), Some(json!({"ids": [3, 4], "before": 1}))).await;
        assert_eq!(r["added"], json!([3, 4]));
        // The list in album order, and filtered to the album.
        let (_, l) = call("GET", format!("/api/photos?album={id}&sort=album"), None).await;
        assert_eq!(l["id"], json!([2, 3, 4, 1]));
        let (_, l) = call("GET", format!("/api/photos?album={id}&rmin=0"), None).await;
        assert_eq!(l["id"], json!([1, 2, 3, 4]), "capture order unless asked for the album's");
        let (_, r) = call("POST", format!("/api/albums/{id}/order"), Some(json!({"ids": [1], "before": 2}))).await;
        let order_batch = r["batch"].as_str().unwrap().to_string();
        let (_, l) = call("GET", format!("/api/photos?album={id}&sort=album"), None).await;
        assert_eq!(l["id"], json!([1, 2, 3, 4]));
        let (_, r) = call("POST", format!("/api/albums/{id}/remove"), Some(json!({"ids": [3]}))).await;
        let remove_batch = r["batch"].as_str().unwrap().to_string();
        let (_, albums) = call("GET", "/api/albums".into(), None).await;
        assert_eq!(albums[0]["count"], 3);
        assert_eq!(albums[0]["cover"]["id"], 1, "no cover set: the first photo");
        // The photo detail says which albums a photo is in.
        let (_, d) = call("GET", "/api/photo/1".into(), None).await;
        assert_eq!(d["albums"], json!([[id, "Best"]]));
        // Undo the removal, then the reorder (read-only mode allows album undos).
        let (st, u) = call("POST", "/api/undo".into(), Some(json!({"batch": remove_batch}))).await;
        assert_eq!(st, StatusCode::OK, "{u}");
        let (_, l) = call("GET", format!("/api/photos?album={id}&sort=album"), None).await;
        assert_eq!(l["id"], json!([1, 2, 3, 4]));
        call("POST", "/api/undo".into(), Some(json!({"batch": order_batch}))).await;
        let (_, l) = call("GET", format!("/api/photos?album={id}&sort=album"), None).await;
        assert_eq!(l["id"], json!([2, 3, 4, 1]));
        // Rename, cover, delete, and undo the delete.
        let (st, _) = call("POST", format!("/api/albums/{id}"), Some(json!({"name": "Best of", "cover": 4}))).await;
        assert_eq!(st, StatusCode::OK);
        let (_, r) = call("DELETE", format!("/api/albums/{id}"), None).await;
        let del_batch = r["batch"].as_str().unwrap().to_string();
        let photos: i64 = state.db.get().unwrap().query_row("SELECT count(*) FROM photos", [], |r| r.get(0)).unwrap();
        assert_eq!(photos, 4, "deleting an album never deletes photos");
        assert_eq!(call("GET", "/api/albums".into(), None).await.1, json!([]));
        call("POST", "/api/undo".into(), Some(json!({"batch": del_batch}))).await;
        let (_, albums) = call("GET", "/api/albums".into(), None).await;
        assert_eq!((albums[0]["name"].as_str(), albums[0]["count"].as_i64(), albums[0]["cover"]["id"].as_i64()), (Some("Best of"), Some(4), Some(4)));
        let (_, l) = call("GET", format!("/api/photos?album={id}&sort=album"), None).await;
        assert_eq!(l["id"], json!([2, 3, 4, 1]));
        // The history shows album changes.
        let (_, j) = call("GET", "/api/journal".into(), None).await;
        assert!(j.as_array().unwrap().iter().any(|e| e["kind"] == "album" && e["summary"].as_str().unwrap().starts_with("Deleted album")));
        assert_eq!(call("POST", "/api/albums/999/add".into(), Some(json!({"ids": [1]}))).await.0, StatusCode::NOT_FOUND);
    }
}
