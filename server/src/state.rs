//! Shared application state.

use crate::config::Config;
use crate::db::Pool;
use crate::images::Images;
use crate::meta::exiftool::ExifTool;
use parking_lot::{Condvar, Mutex, RwLock};
use serde::Serialize;
use std::collections::HashSet;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Sender;

#[derive(Debug, Clone, Default, Serialize)]
pub struct Progress {
    pub phase: String,
    pub folders_total: usize,
    pub folders_done: usize,
    pub photos_indexed: u64,
    pub current: Option<String>,
    pub last_full_scan: Option<i64>,
    pub thumbs_pending: usize,
    pub library_ok: bool,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Progress(Progress),
    Folder { id: i64 },
    Tree,
    Photos { ids: Vec<i64> },
    /// The saved collections changed (in another tab or on another device).
    Collections,
    /// The albums changed: their list, counts or members.
    Albums,
    /// Progress of search by content: photos looked at, of how many.
    Visual { phase: String, done: i64, total: i64 },
    /// Progress of culling's measurements, and the photos just measured.
    Cull { phase: String, done: i64, total: i64, ids: Vec<i64> },
    /// Progress of a background change.
    Job(crate::ops::JobStatus),
    /// The archive integrity problems changed: how many of each there are now.
    Integrity { changed: i64, missing: i64, unreadable: i64 },
    /// Full-quality thumbnails replaced the drafts shown for these photos.
    Thumbs { ids: Vec<i64> },
    /// Who is in these photos changed (empty: many photos, reload what you show).
    People { photos: Vec<i64> },
    /// Progress of the face and pet analysis.
    PeopleStatus(crate::people::Status),
    /// New visual fingerprints for the Duplicates report (it refreshes its counts).
    Dupes,
    /// The AI engine: downloading its models, starting, ready, or stopped.
    Ml(crate::ml::MlStatus),
    /// The advanced settings changed: series and possible rejects may have too.
    Tuning,
}

impl Event {
    /// The counter of this type of message in debug mode.
    fn counter(&self) -> &'static str {
        match self {
            Event::Progress(_) => "sse.progress",
            Event::Folder { .. } => "sse.folder",
            Event::Tree => "sse.tree",
            Event::Photos { .. } => "sse.photos",
            Event::Collections => "sse.collections",
            Event::Albums => "sse.albums",
            Event::Visual { .. } => "sse.visual",
            Event::Cull { .. } => "sse.cull",
            Event::Job(_) => "sse.job",
            Event::Integrity { .. } => "sse.integrity",
            Event::Thumbs { .. } => "sse.thumbs",
            Event::People { .. } => "sse.people",
            Event::PeopleStatus(_) => "sse.people_status",
            Event::Dupes => "sse.dupes",
            Event::Ml(_) => "sse.ml",
            Event::Tuning => "sse.tuning",
        }
    }
}

pub enum ScanMsg {
    Folder(i64),
    Full,
}

pub struct AppState {
    pub cfg: Config,
    pub db: Pool,
    pub events: tokio::sync::broadcast::Sender<Event>,
    pub exiftool: tokio::sync::Mutex<ExifTool>,
    /// Serialises every change to the photo library (sidecar writes and moves).
    pub write_lock: tokio::sync::Mutex<()>,
    pub progress: RwLock<Progress>,
    pub scan_tx: Mutex<Option<Sender<ScanMsg>>>,
    /// Folders currently being scanned (two scans of one folder never overlap).
    pub busy: Mutex<HashSet<i64>>,
    pub busy_cv: Condvar,
    pub images: Images,
    pub readonly: AtomicBool,
    /// The trigram index covers every photo, so search can match substrings.
    pub trigram: AtomicBool,
    pub session_key: [u8; 32],
    /// RAW and HEIC decoders available to this build.
    pub decoders: crate::native::Decoders,
    /// Background changes, running and recently finished, by id.
    pub jobs: Mutex<std::collections::HashMap<String, crate::ops::JobStatus>>,
    /// Track files uploaded for geotagging, by id (kept for a few hours).
    pub tracks: Mutex<std::collections::HashMap<String, crate::geotag::StoredTrack>>,
    pub integrity: crate::integrity::Integrity,
    /// Faces, pets and who they are.
    pub people: crate::people::People,
    /// The perceptual-hash worker behind the Duplicates report.
    pub dupes: crate::dupes::Dupes,
    /// Search by content: image embeddings of the photos.
    pub visual: crate::visual::Visual,
    /// Culling: sharpness, eyes and exposure of the photos.
    pub cull: crate::cull::Cull,
    /// The AI engine (faces, pets, search, eyes): its models, process and status.
    pub ml: crate::ml::Ml,
}

impl AppState {
    pub fn emit(&self, e: Event) {
        if crate::telemetry::on() {
            // Messages on the event stream, by type (each one goes to every open browser).
            crate::telemetry::count(e.counter(), 1);
        }
        let _ = self.events.send(e);
    }

    pub fn is_readonly(&self) -> bool {
        self.readonly.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn trigram_ready(&self) -> bool {
        self.trigram.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn request_scan(&self, msg: ScanMsg) {
        if let Some(tx) = self.scan_tx.lock().as_ref() {
            let _ = tx.send(msg);
        }
    }

    /// Run `f` while holding the per-folder scan lock.
    pub fn with_folder_lock<T>(&self, folder_id: i64, f: impl FnOnce() -> T) -> T {
        {
            let mut b = self.busy.lock();
            while b.contains(&folder_id) {
                self.busy_cv.wait(&mut b);
            }
            b.insert(folder_id);
        }
        struct Guard<'a>(&'a AppState, i64);
        impl Drop for Guard<'_> {
            fn drop(&mut self) {
                self.0.busy.lock().remove(&self.1);
                self.0.busy_cv.notify_all();
            }
        }
        let _g = Guard(self, folder_id);
        f()
    }
}
