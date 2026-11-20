//! People and pets: faces, dogs and cats found by the companion ML service
//! (`hyperfocal-ml`), grouped by who they are, named by you, and written to the XMP
//! sidecars (person keywords, MWG face regions, IPTC "person shown") once confirmed.
//!
//! The ML service only ever sees upright JPEGs and answers boxes and embeddings. The
//! rest happens here:
//! - `worker`: picks the photos to analyse (the ones you are looking at first, then the
//!   newest), prepares and sends them, stores what comes back, and reads the face
//!   regions other apps wrote (Lightroom, digiKam), so names you gave there carry over.
//! - `engine` and `index`: match each new face or animal to a person, a pet or an
//!   unnamed group, with thresholds that tighten for poor faces, and form new groups.
//! - `xmp`: what a photo's sidecar should say about the people and pets in it.
//! - `api`: the People & pets view: naming, merging, confirming, rejecting.
//!
//! The database keeps the detections and the groups; the sidecars keep what you
//! confirmed, so the names survive a lost index (they are read back on analysis).

pub mod api;
pub mod engine;
pub mod index;
pub mod ml;
pub mod regions;
pub mod worker;
pub mod xmp;

use anyhow::Result;
use parking_lot::{Condvar, Mutex, RwLock};
use rusqlite::Connection;
use serde::Serialize;
use std::collections::{HashSet, VecDeque};

/// Detection kinds. A pet subject's `species` is the kind of its detections.
pub const FACE: i64 = 1;
pub const DOG: i64 = 2;
pub const CAT: i64 = 3;

/// Subject kinds.
pub const PERSON: i64 = 1;
pub const PET: i64 = 2;

/// Detection states.
/// Found, perhaps matched to a subject (a suggestion), not confirmed.
pub const AUTO: i64 = 0;
/// Confirmed by you (named, assigned or accepted): written to the sidecar.
pub const CONFIRMED: i64 = 1;
/// Not a face, or not of interest: left out of everything.
pub const IGNORED: i64 = 3;

/// Detection sources.
pub const SRC_DETECTED: i64 = 0;
/// A named region another app wrote that matches no detection.
pub const SRC_XMP: i64 = 1;

/// Embedding length (both models).
pub const DIM: usize = 512;

pub fn kind_name(kind: i64) -> &'static str {
    match kind {
        DOG => "dog",
        CAT => "cat",
        _ => "face",
    }
}

/// The keyword a subject gets in the sidecars: `People|Name` or `Pets|Name`.
pub fn keyword(subject_kind: i64, name: &str) -> String {
    let leaf = name.replace('|', "/");
    if subject_kind == PET { format!("Pets|{leaf}") } else { format!("People|{leaf}") }
}

pub fn migrate(c: &Connection) -> Result<()> {
    c.execute_batch(
        r#"
        -- A person, a pet or an unnamed group of faces or animals.
        -- (AUTOINCREMENT: an id is never reused, so a stale id in a viewer or a cache
        -- never points at someone else.)
        CREATE TABLE IF NOT EXISTS subjects(
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            kind INTEGER NOT NULL,
            species INTEGER NOT NULL DEFAULT 1,
            name TEXT,
            hidden INTEGER NOT NULL DEFAULT 0,
            created INTEGER NOT NULL,
            updated INTEGER NOT NULL
        );
        CREATE UNIQUE INDEX IF NOT EXISTS subjects_name ON subjects(kind, name COLLATE NOCASE) WHERE name IS NOT NULL;

        -- A face, dog or cat in a photo: its box (upright image, 0..1), how good a view
        -- it is, its embedding (int8) and who it is.
        CREATE TABLE IF NOT EXISTS detections(
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            photo_id INTEGER NOT NULL REFERENCES photos(id) ON DELETE CASCADE,
            kind INTEGER NOT NULL,
            x REAL NOT NULL, y REAL NOT NULL, w REAL NOT NULL, h REAL NOT NULL,
            score REAL NOT NULL,
            quality REAL NOT NULL,
            emb BLOB,
            subject_id INTEGER REFERENCES subjects(id) ON DELETE SET NULL,
            state INTEGER NOT NULL DEFAULT 0,
            sim REAL,
            source INTEGER NOT NULL DEFAULT 0,
            created INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS detections_photo ON detections(photo_id);
        CREATE INDEX IF NOT EXISTS detections_subject ON detections(subject_id, state, photo_id) WHERE subject_id IS NOT NULL;

        -- Detections deleted (with their photo, folder or a new analysis), however that
        -- happened: the worker takes them out of the in-memory index and the crops.
        CREATE TABLE IF NOT EXISTS detections_gone(id INTEGER PRIMARY KEY);
        CREATE TRIGGER IF NOT EXISTS detections_gone_t AFTER DELETE ON detections BEGIN
            INSERT OR IGNORE INTO detections_gone(id) VALUES(old.id);
        END;

        -- "This face is not that person": never suggested again.
        CREATE TABLE IF NOT EXISTS detection_rejects(
            detection_id INTEGER NOT NULL REFERENCES detections(id) ON DELETE CASCADE,
            subject_id INTEGER NOT NULL REFERENCES subjects(id) ON DELETE CASCADE,
            PRIMARY KEY(detection_id, subject_id)
        ) WITHOUT ROWID;

        -- Which photos were analysed, as which file (size, date, orientation) and with
        -- which models.
        CREATE TABLE IF NOT EXISTS analysis(
            photo_id INTEGER PRIMARY KEY REFERENCES photos(id) ON DELETE CASCADE,
            size INTEGER NOT NULL,
            mtime INTEGER NOT NULL,
            orientation INTEGER NOT NULL,
            models TEXT NOT NULL,
            at INTEGER NOT NULL,
            error TEXT,
            -- Size of the analysed (upright) image.
            w INTEGER NOT NULL DEFAULT 0,
            h INTEGER NOT NULL DEFAULT 0
        );
        "#,
    )?;
    Ok(())
}

/// What the People & pets view shows about the analysis.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Status {
    /// An ML service is configured (HYPERFOCAL_ML_URL).
    pub enabled: bool,
    /// off, connecting, offline, analysing, idle, paused, grouping, error
    pub phase: String,
    /// What the service reported: version, device, models.
    pub service: Option<serde_json::Value>,
    pub error: Option<String>,
    pub analyzed: i64,
    pub pending: i64,
    pub failed: i64,
    /// Photos a minute, recently.
    pub rate: f64,
    pub current: Option<String>,
    /// Re-grouping progress (0..1) while it runs.
    pub regroup: Option<f64>,
}

#[derive(Default)]
pub struct Ctl {
    pub paused: bool,
    /// Photos to analyse before anything else (the ones on screen).
    pub priority: VecDeque<i64>,
    pub priority_set: HashSet<i64>,
    /// Photos whose sidecar may hold new names (read without the ML service).
    pub resync: VecDeque<i64>,
    /// Photos whose sidecar an undo restored: their names are read back exactly.
    pub exact: VecDeque<i64>,
    pub regroup: bool,
    pub wake: bool,
}

pub struct People {
    pub status: RwLock<Status>,
    pub ctl: Mutex<Ctl>,
    pub cv: Condvar,
    /// The embedding index, loaded once the worker starts.
    pub engine: Mutex<Option<engine::Engine>>,
    /// Face and animal crops (a cache database next to the thumbnails).
    pub crops: worker::Crops,
    /// Photos whose sidecars wait to be brought in line (see `xmp::spawn_writer`).
    pub pending: Mutex<xmp::Pending>,
    pub write_wake: tokio::sync::Notify,
}

impl People {
    pub fn new(cfg: &crate::config::Config) -> Result<People> {
        Ok(People {
            status: RwLock::new(Status { enabled: cfg.ml_url.is_some(), phase: "off".into(), ..Default::default() }),
            ctl: Mutex::new(Ctl::default()),
            cv: Condvar::new(),
            engine: Mutex::new(None),
            crops: worker::Crops::open(&cfg.cache_dir.join("crops.db"))?,
            pending: Mutex::new(xmp::Pending::default()),
            write_wake: tokio::sync::Notify::new(),
        })
    }

    pub fn wake(&self) {
        self.ctl.lock().wake = true;
        self.cv.notify_all();
    }

    /// Analyse these photos next (the ones the viewer is showing).
    pub fn prioritize(&self, ids: &[i64]) {
        let mut c = self.ctl.lock();
        for &id in ids.iter().rev() {
            if c.priority_set.insert(id) {
                c.priority.push_front(id);
            }
        }
        while c.priority.len() > 2000 {
            if let Some(id) = c.priority.pop_back() {
                c.priority_set.remove(&id);
            }
        }
        c.wake = true;
        drop(c);
        self.cv.notify_all();
    }

    /// The scanner saw new or changed photos (or sidecars): read their names again.
    pub fn changed(&self, ids: &[i64]) {
        if ids.is_empty() {
            return;
        }
        let mut c = self.ctl.lock();
        c.resync.extend(ids.iter().copied());
        c.wake = true;
        drop(c);
        self.cv.notify_all();
    }
}

/// Things that changed, for the UI's live updates.
pub fn emit_changed(state: &crate::state::AppState, photos: Vec<i64>) {
    state.emit(crate::state::Event::People { photos });
}
