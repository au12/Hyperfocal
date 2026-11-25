//! SQLite index. The database is only an index: everything in it can be rebuilt
//! from the photo folders and their XMP sidecars at any time (the integrity
//! monitor's baselines and the saved collections are the exception: they live only
//! here, and `/config` is backed up like any app data).

use anyhow::{Context, Result};
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;

pub type Pool = r2d2::Pool<SqliteConnectionManager>;

/// 1: the original schema. 2: `photos.added_at`, covering indexes for the common
/// filters, the trigram index, saved collections and the integrity monitor. 3: people
/// and pets (detections, subjects, analysis state). 4: visual fingerprints for the
/// Duplicates report. 5: albums.
const SCHEMA_VERSION: i64 = 5;

pub fn open(path: &Path) -> Result<Pool> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let manager = SqliteConnectionManager::file(path).with_init(|c| {
        c.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA foreign_keys=ON;
             PRAGMA busy_timeout=15000;
             PRAGMA temp_store=MEMORY;
             PRAGMA cache_size=-32768;
             PRAGMA mmap_size=268435456;
             PRAGMA analysis_limit=2000;",
        )?;
        // Debug mode: every statement's time and full-scan steps.
        crate::telemetry::sql::install(c);
        Ok(())
    });
    let pool = r2d2::Pool::builder().max_size(12).build(manager).context("opening database")?;
    let conn = pool.get()?;
    migrate(&conn)?;
    drop(conn);
    Ok(pool)
}

pub(crate) fn has_column(c: &Connection, table: &str, col: &str) -> Result<bool> {
    let mut st = c.prepare(&format!("PRAGMA table_info({table})"))?;
    let names = st.query_map([], |r| r.get::<_, String>(1))?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(names.iter().any(|n| n == col))
}

fn table_exists(c: &Connection, name: &str) -> Result<bool> {
    Ok(c.query_row("SELECT 1 FROM sqlite_master WHERE name = ?1", [name], |_| Ok(())).optional()?.is_some())
}

fn migrate(c: &Connection) -> Result<()> {
    c.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT);

        CREATE TABLE IF NOT EXISTS folders(
            id INTEGER PRIMARY KEY,
            parent_id INTEGER REFERENCES folders(id) ON DELETE CASCADE,
            path TEXT NOT NULL UNIQUE,
            name TEXT NOT NULL,
            is_reject INTEGER NOT NULL DEFAULT 0,
            scanned_at INTEGER NOT NULL DEFAULT 0,
            photo_count INTEGER NOT NULL DEFAULT 0,
            seen INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS folders_parent ON folders(parent_id);

        CREATE TABLE IF NOT EXISTS photos(
            id INTEGER PRIMARY KEY,
            folder_id INTEGER NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
            name TEXT NOT NULL,
            stem TEXT NOT NULL,
            ext TEXT NOT NULL,
            kind INTEGER NOT NULL,
            size INTEGER NOT NULL,
            mtime INTEGER NOT NULL,
            sidecar TEXT,
            sidecar_mtime INTEGER,
            pair_of INTEGER,
            companion TEXT,
            width INTEGER,
            height INTEGER,
            orientation INTEGER NOT NULL DEFAULT 1,
            taken INTEGER,
            taken_str TEXT,
            make TEXT, model TEXT, lens TEXT,
            focal REAL, fnum REAL, exposure REAL, iso INTEGER, bias REAL,
            rating INTEGER NOT NULL DEFAULT 0,
            label TEXT,
            lcolor TEXT,
            pick INTEGER NOT NULL DEFAULT 0,
            title TEXT, description TEXT, creator TEXT, rights TEXT,
            keywords TEXT, hkeywords TEXT,
            lat REAL, lon REAL, alt REAL,
            city TEXT, state TEXT, country TEXT, country_code TEXT, sublocation TEXT,
            meta_err TEXT,
            indexed_at INTEGER NOT NULL DEFAULT 0,
            UNIQUE(folder_id, name)
        );
        CREATE INDEX IF NOT EXISTS photos_folder ON photos(folder_id);
        CREATE INDEX IF NOT EXISTS photos_taken ON photos(taken);
        CREATE INDEX IF NOT EXISTS photos_geo ON photos(lat, lon) WHERE lat IS NOT NULL;
        CREATE INDEX IF NOT EXISTS photos_pair ON photos(pair_of) WHERE pair_of IS NOT NULL;

        CREATE TABLE IF NOT EXISTS photo_keywords(
            photo_id INTEGER NOT NULL REFERENCES photos(id) ON DELETE CASCADE,
            kw TEXT NOT NULL COLLATE NOCASE,
            PRIMARY KEY(photo_id, kw)
        ) WITHOUT ROWID;
        CREATE INDEX IF NOT EXISTS photo_keywords_kw ON photo_keywords(kw);

        CREATE VIRTUAL TABLE IF NOT EXISTS photos_fts USING fts5(
            name, path, title, description, keywords, place, camera,
            content='', contentless_delete=1, tokenize='unicode61 remove_diacritics 2'
        );

        CREATE TABLE IF NOT EXISTS journal(
            id INTEGER PRIMARY KEY,
            batch TEXT NOT NULL,
            ts INTEGER NOT NULL,
            kind TEXT NOT NULL,
            photo_id INTEGER,
            summary TEXT,
            data TEXT NOT NULL,
            undone INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS journal_batch ON journal(batch);

        CREATE TABLE IF NOT EXISTS settings(key TEXT PRIMARY KEY, value TEXT NOT NULL);
        "#,
    )?;
    let v: Option<String> = c.query_row("SELECT value FROM meta WHERE key='schema'", [], |r| r.get(0)).optional()?;
    let version: i64 = v.as_deref().and_then(|s| s.parse().ok()).unwrap_or(0);

    // ---- version 2
    if !has_column(c, "photos", "added_at")? {
        // Existing photos count as old (their real import time is unknown).
        c.execute_batch("ALTER TABLE photos ADD COLUMN added_at INTEGER NOT NULL DEFAULT 0;")?;
    }
    let tri_existed = table_exists(c, "photos_tri")?;
    c.execute_batch(
        r#"
        -- Covering indexes for what the UI asks of a large library: the photos a list
        -- shows (pair_of IS NULL), in capture order or by name, per folder, by camera,
        -- lens and label (filters and facets), and on the map.
        CREATE INDEX IF NOT EXISTS photos_vis_order ON photos(COALESCE(taken, mtime / 1000), name COLLATE NOCASE) WHERE pair_of IS NULL;
        CREATE INDEX IF NOT EXISTS photos_vis_folder ON photos(folder_id, taken) WHERE pair_of IS NULL;
        CREATE INDEX IF NOT EXISTS photos_vis_name ON photos(name COLLATE NOCASE) WHERE pair_of IS NULL;
        CREATE INDEX IF NOT EXISTS photos_vis_model ON photos(model) WHERE pair_of IS NULL;
        CREATE INDEX IF NOT EXISTS photos_vis_lens ON photos(lens) WHERE pair_of IS NULL;
        CREATE INDEX IF NOT EXISTS photos_vis_label ON photos(lcolor) WHERE pair_of IS NULL;
        CREATE INDEX IF NOT EXISTS photos_vis_geo ON photos(lat, lon, rating, folder_id) WHERE pair_of IS NULL AND lat IS NOT NULL;
        CREATE INDEX IF NOT EXISTS photos_added ON photos(added_at) WHERE added_at > 0;
        -- Tells the planner how few reject folders there are ("All rejected").
        CREATE INDEX IF NOT EXISTS folders_reject ON folders(is_reject) WHERE is_reject = 1;

        -- Substring search: every three-character sequence of the searchable text.
        CREATE VIRTUAL TABLE IF NOT EXISTS photos_tri USING fts5(
            t, content='', contentless_delete=1, tokenize='trigram remove_diacritics 1'
        );

        CREATE TABLE IF NOT EXISTS collections(
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            spec TEXT NOT NULL,
            position INTEGER NOT NULL DEFAULT 0,
            created INTEGER NOT NULL,
            updated INTEGER NOT NULL
        );

        -- Archive integrity: one row per original image file, by path. Rows outlive the
        -- index rows of their photos, so a vanished file can still be reported.
        CREATE TABLE IF NOT EXISTS originals(
            id INTEGER PRIMARY KEY,
            folder TEXT NOT NULL,
            name TEXT NOT NULL,
            size INTEGER NOT NULL,
            mtime INTEGER NOT NULL,
            scope INTEGER NOT NULL,
            hash TEXT NOT NULL,
            first_seen INTEGER NOT NULL,
            verified_at INTEGER NOT NULL,
            status INTEGER NOT NULL DEFAULT 0,
            status_at INTEGER,
            detail TEXT,
            new_hash TEXT,
            notified INTEGER NOT NULL DEFAULT 0,
            UNIQUE(folder, name)
        );
        CREATE INDEX IF NOT EXISTS originals_hash ON originals(hash);
        CREATE INDEX IF NOT EXISTS originals_due ON originals(verified_at) WHERE status = 0;
        CREATE INDEX IF NOT EXISTS originals_problems ON originals(status, status_at) WHERE status != 0;
        "#,
    )?;
    // ---- version 3
    crate::people::migrate(c)?;
    // ---- version 4
    crate::dupes::migrate(c)?;
    // ---- version 5
    crate::albums::migrate(c)?;
    crate::immich::migrate(c)?;
    crate::visual::migrate(c)?;
    crate::cull::migrate(c)?;
    if !tri_existed {
        // A new (or empty) index fills the trigram table as photos are indexed; an
        // existing one is filled in the background (`backfill_trigram`).
        let photos: i64 = c.query_row("SELECT count(*) FROM photos", [], |r| r.get(0))?;
        if photos == 0 {
            set_meta(c, "fts_tri", "ready")?;
        }
    }
    if version != SCHEMA_VERSION {
        c.execute(
            "INSERT INTO meta(key, value) VALUES('schema', ?1) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![SCHEMA_VERSION.to_string()],
        )?;
    }
    Ok(())
}

pub fn set_meta(c: &Connection, key: &str, value: &str) -> Result<()> {
    c.execute("INSERT INTO meta(key, value) VALUES(?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value", params![key, value])?;
    Ok(())
}

pub fn get_meta(c: &Connection, key: &str) -> Option<String> {
    c.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0)).optional().ok().flatten()
}

/// Whether the trigram (substring) index covers every photo.
pub fn trigram_ready(c: &Connection) -> bool {
    get_meta(c, "fts_tri").as_deref() == Some("ready")
}

/// Fill the trigram index for photos indexed before it existed, in small transactions
/// so the scanner and edits are never held up for long. Returns the photos indexed.
pub fn backfill_trigram(pool: &Pool) -> Result<usize> {
    let mut last = 0i64;
    let mut done = 0usize;
    loop {
        let mut c = pool.get()?;
        let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let rows: Vec<(i64, crate::model::PhotoRec, String)> = {
            let mut st = tx.prepare(&format!(
                "SELECT {}, f.path FROM photos p JOIN folders f ON f.id = p.folder_id WHERE p.id > ?1 ORDER BY p.id LIMIT 2000",
                crate::repo::PHOTO_COLS
            ))?;
            let it = st.query_map([last], |r| Ok((r.get(0)?, crate::repo::photo_from_row(r)?, r.get(43)?)))?;
            it.collect::<rusqlite::Result<_>>()?
        };
        if rows.is_empty() {
            set_meta(&tx, "fts_tri", "ready")?;
            tx.commit()?;
            return Ok(done);
        }
        for (id, rec, path) in &rows {
            crate::repo::index_trigram(&tx, *id, rec, path)?;
            last = *id;
        }
        done += rows.len();
        tx.commit()?;
    }
}

/// Refresh the query planner's statistics (cheap; only tables that changed enough).
pub fn optimize(c: &Connection) -> Result<()> {
    c.execute_batch("PRAGMA optimize=0x10002;")?;
    Ok(())
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// A free-text search as FTS5 queries. Words of four or more characters (three or more
/// with a digit, like a fragment of a file number) match anywhere in the text through
/// the trigram index; shorter words match the start of a word.
#[derive(Debug, Default, PartialEq)]
pub struct Search {
    /// For `photos_fts` (unicode61, prefix matches).
    pub prefix: Option<String>,
    /// For `photos_tri` (trigram, substring matches).
    pub substring: Option<String>,
}

pub fn search_query(q: &str, trigram: bool) -> Search {
    let mut prefix = Vec::new();
    let mut substring = Vec::new();
    for w in q.split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-').filter(|w| !w.is_empty()) {
        let w = w.replace('"', "");
        let n = w.chars().count();
        let digit = w.chars().any(|c| c.is_ascii_digit());
        if trigram && (n >= 4 || (n >= 3 && digit)) {
            substring.push(format!("\"{w}\""));
        } else {
            prefix.push(format!("\"{w}\"*"));
        }
    }
    let join = |v: Vec<String>| (!v.is_empty()).then(|| v.join(" "));
    Search { prefix: join(prefix), substring: join(substring) }
}

/// Turn a free-text search into a safe FTS5 query (every word becomes a prefix match).
pub fn fts_query(q: &str) -> Option<String> {
    search_query(q, false).prefix
}

pub fn get_setting(c: &Connection, key: &str) -> Option<String> {
    c.query_row("SELECT value FROM settings WHERE key=?1", [key], |r| r.get(0)).optional().ok().flatten()
}

pub fn set_setting(c: &Connection, key: &str, value: &str) -> Result<()> {
    c.execute(
        "INSERT INTO settings(key, value) VALUES(?1, ?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![key, value],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_words() {
        assert_eq!(search_query("", true), Search::default());
        let s = search_query("cat Lisbon 042 IMG_1234 f/2", true);
        assert_eq!(s.prefix.as_deref(), Some("\"cat\"* \"f\"* \"2\"*"));
        assert_eq!(s.substring.as_deref(), Some("\"Lisbon\" \"042\" \"IMG_1234\""));
        // Without the trigram index everything is a prefix match.
        assert_eq!(search_query("Lisbon 042", false).prefix.as_deref(), Some("\"Lisbon\"* \"042\"*"));
        assert_eq!(search_query("a\"b", true).prefix.as_deref(), Some("\"a\"* \"b\"*"));
    }

    #[test]
    fn migrates_a_version_1_index() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("old.db");
        {
            let c = Connection::open(&p).unwrap();
            // The version 1 schema, with a photo in it.
            c.execute_batch(
                "CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT);
                 INSERT INTO meta VALUES('schema', '1');
                 CREATE TABLE folders(id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES folders(id) ON DELETE CASCADE, path TEXT NOT NULL UNIQUE,
                   name TEXT NOT NULL, is_reject INTEGER NOT NULL DEFAULT 0, scanned_at INTEGER NOT NULL DEFAULT 0,
                   photo_count INTEGER NOT NULL DEFAULT 0, seen INTEGER NOT NULL DEFAULT 0);
                 CREATE TABLE photos(id INTEGER PRIMARY KEY, folder_id INTEGER NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
                   name TEXT NOT NULL, stem TEXT NOT NULL, ext TEXT NOT NULL, kind INTEGER NOT NULL, size INTEGER NOT NULL,
                   mtime INTEGER NOT NULL, sidecar TEXT, sidecar_mtime INTEGER, pair_of INTEGER, companion TEXT, width INTEGER,
                   height INTEGER, orientation INTEGER NOT NULL DEFAULT 1, taken INTEGER, taken_str TEXT, make TEXT, model TEXT,
                   lens TEXT, focal REAL, fnum REAL, exposure REAL, iso INTEGER, bias REAL, rating INTEGER NOT NULL DEFAULT 0,
                   label TEXT, lcolor TEXT, pick INTEGER NOT NULL DEFAULT 0, title TEXT, description TEXT, creator TEXT,
                   rights TEXT, keywords TEXT, hkeywords TEXT, lat REAL, lon REAL, alt REAL, city TEXT, state TEXT, country TEXT,
                   country_code TEXT, sublocation TEXT, meta_err TEXT, indexed_at INTEGER NOT NULL DEFAULT 0, UNIQUE(folder_id, name));
                 INSERT INTO folders(id, path, name) VALUES(1, '', 'Library'), (2, '2024/Trip', 'Trip');
                 INSERT INTO photos(folder_id, name, stem, ext, kind, size, mtime, title) VALUES(2, 'IMG_1.CR3', 'IMG_1', 'cr3', 1, 10, 0, 'Lisboa Alfama');",
            )
            .unwrap();
        }
        let pool = open(&p).unwrap();
        let c = pool.get().unwrap();
        assert_eq!(get_meta(&c, "schema").as_deref(), Some("5"));
        assert!(has_column(&c, "photos", "added_at").unwrap());
        assert!(has_column(&c, "detections", "emb").unwrap());
        assert!(has_column(&c, "phashes", "hash").unwrap());
        assert!(table_exists(&c, "dupes_ignored").unwrap());
        assert!(has_column(&c, "album_photos", "pos").unwrap());
        // The trigram index of an existing library is filled in the background.
        assert!(!trigram_ready(&c));
        drop(c);
        assert_eq!(backfill_trigram(&pool).unwrap(), 1);
        let c = pool.get().unwrap();
        assert!(trigram_ready(&c));
        let n: i64 = c.query_row("SELECT count(*) FROM photos_tri WHERE photos_tri MATCH '\"lfam\"'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        // Opening again changes nothing.
        drop(c);
        drop(pool);
        let pool = open(&p).unwrap();
        assert!(trigram_ready(&pool.get().unwrap()));
    }

    #[test]
    fn new_index_needs_no_backfill() {
        let t = tempfile::tempdir().unwrap();
        let pool = open(&t.path().join("new.db")).unwrap();
        assert!(trigram_ready(&pool.get().unwrap()));
    }
}
