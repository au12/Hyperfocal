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
    /// Progress of a background change.
    Job(crate::ops::JobStatus),
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
}

impl AppState {
    pub fn emit(&self, e: Event) {
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
