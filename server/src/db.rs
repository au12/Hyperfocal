//! SQLite index. The database is only an index: everything in it can be rebuilt
//! from the photo folders and their XMP sidecars at any time.

use anyhow::{Context, Result};
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;

pub type Pool = r2d2::Pool<SqliteConnectionManager>;

const SCHEMA_VERSION: i64 = 1;

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
             PRAGMA cache_size=-32768;",
        )
    });
    let pool = r2d2::Pool::builder().max_size(12).build(manager).context("opening database")?;
    let conn = pool.get()?;
    migrate(&conn)?;
    drop(conn);
    Ok(pool)
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
    let v: Option<String> =
        c.query_row("SELECT value FROM meta WHERE key='schema'", [], |r| r.get(0)).optional()?;
    if v.is_none() {
        c.execute("INSERT INTO meta(key, value) VALUES('schema', ?1)", params![SCHEMA_VERSION.to_string()])?;
    }
    Ok(())
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Turn a free-text search into a safe FTS5 query (every word becomes a prefix match).
pub fn fts_query(q: &str) -> Option<String> {
    let words: Vec<String> = q
        .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
        .filter(|w| !w.is_empty())
        .map(|w| format!("\"{}\"*", w.replace('"', "")))
        .collect();
    if words.is_empty() { None } else { Some(words.join(" ")) }
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
