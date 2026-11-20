//! Duplicates: exact copies, near-duplicates and series, each group with a suggested
//! keeper and a way to move the others to `_Rejected` (journaled, undoable).
//!
//! - Exact copies come from the integrity monitor's content hashes (`originals`), which
//!   for JPEG, HEIC, TIFF and DNG cover only the image data: copies that differ only in
//!   their metadata still match, where a whole-file hash would miss them.
//! - Near-duplicates (resized, recompressed, re-exported, lightly edited copies, and
//!   frames that hardly differ) come from a 64-bit perceptual hash of every photo's
//!   thumbnail (`phash`), kept in `phashes` by the background `worker` and matched by
//!   Hamming distance (`mih`).
//! - Series are photos one camera took in quick succession (`bursts`).
//!
//! `report` forms the groups for the library, a folder, or a folder against the rest of
//! the library; `api` serves them. Nothing here writes to the photo library: rejecting
//! goes through the usual move (`ops::reject`).

pub mod api;
pub mod bursts;
pub mod mih;
pub mod phash;
pub mod report;
pub mod worker;

use anyhow::Result;
use parking_lot::{Condvar, Mutex, RwLock};
use rusqlite::Connection;
use serde::Serialize;
use std::collections::{HashSet, VecDeque};

pub fn migrate(c: &Connection) -> Result<()> {
    c.execute_batch(
        r#"
        -- The perceptual hash of each photo's thumbnail, and of which file (size, date,
        -- orientation) it was made. hash: the 64 bits as a signed integer; 0 for an image
        -- too flat to compare; NULL when the photo could not be read (error says why).
        -- sharp: a focus estimate for picking a frame of a series.
        CREATE TABLE IF NOT EXISTS phashes(
            photo_id INTEGER PRIMARY KEY REFERENCES photos(id) ON DELETE CASCADE,
            size INTEGER NOT NULL,
            mtime INTEGER NOT NULL,
            orientation INTEGER NOT NULL,
            hash INTEGER,
            sharp REAL,
            at INTEGER NOT NULL,
            error TEXT
        );

        -- "Not duplicates": pairs of photos (a < b) never grouped together again.
        CREATE TABLE IF NOT EXISTS dupes_ignored(
            a INTEGER NOT NULL REFERENCES photos(id) ON DELETE CASCADE,
            b INTEGER NOT NULL REFERENCES photos(id) ON DELETE CASCADE,
            at INTEGER NOT NULL,
            PRIMARY KEY(a, b)
        ) WITHOUT ROWID;
        CREATE INDEX IF NOT EXISTS dupes_ignored_b ON dupes_ignored(b);

        -- Exact copies: the integrity hashes of the originals as they are, so finding the
        -- hashes shared by two files reads only this index (a tenth of the time).
        CREATE INDEX IF NOT EXISTS originals_hash_ok ON originals(hash) WHERE status = 0;
        "#,
    )?;
    Ok(())
}

/// What the Duplicates report shows about the background hashing.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Status {
    /// The worker runs (HYPERFOCAL_DUPES).
    pub enabled: bool,
    /// off, waiting (for the scan or the thumbnails), hashing, idle
    pub phase: String,
    /// Photos still to hash, as of the start of this pass (counted down as it goes).
    pub pending: i64,
    /// Photos hashed in this pass.
    pub done: i64,
    pub current: Option<String>,
}

#[derive(Default)]
pub struct Ctl {
    /// Photos to hash before the rest (a folder the report was asked about).
    pub priority: VecDeque<i64>,
    pub priority_set: HashSet<i64>,
    pub wake: bool,
}

pub struct Dupes {
    pub status: RwLock<Status>,
    pub ctl: Mutex<Ctl>,
    pub cv: Condvar,
}

impl Dupes {
    pub fn new(cfg: &crate::config::Config) -> Dupes {
        Dupes { status: RwLock::new(Status { enabled: cfg.dupes, phase: "off".into(), ..Default::default() }), ctl: Mutex::new(Ctl::default()), cv: Condvar::new() }
    }

    pub fn wake(&self) {
        self.ctl.lock().wake = true;
        self.cv.notify_all();
    }

    /// Hash these photos next.
    pub fn prioritize(&self, ids: &[i64]) {
        if ids.is_empty() {
            return;
        }
        let mut c = self.ctl.lock();
        for &id in ids.iter().rev() {
            if c.priority_set.insert(id) {
                c.priority.push_front(id);
            }
        }
        while c.priority.len() > 50_000 {
            if let Some(id) = c.priority.pop_back() {
                c.priority_set.remove(&id);
            }
        }
        c.wake = true;
        drop(c);
        self.cv.notify_all();
    }
}
