//! Search by content: an image embedding of every photo (SigLIP 2, from the ML service,
//! made from its thumbnail), for
//! - finding photos by a description: "dog on the beach", "red car at night", in any of
//!   the languages SigLIP 2 learned;
//! - photos like a given one ("Find similar");
//! - duplicates that pixel fingerprints miss: crops, edits, other renderings, mirror
//!   images (see `dupes::report`).
//!
//! Embeddings are kept as int8 vectors (768 bytes a photo) in the index and in memory:
//! a search compares the query with every photo (AVX2), about 50 ms for 300,000 photos.
//! A photo's embedding is made again when its file changes, and every one when the ML
//! service brings another image model.

pub mod worker;

use crate::people::index::{dot, norm, quantize, to_blob};
use crate::state::AppState;
use anyhow::{Result, anyhow};
use parking_lot::{Condvar, Mutex, RwLock};
use rusqlite::{Connection, params};
use serde::Serialize;
use std::collections::{HashMap, VecDeque};

pub const DIM: usize = 768;

/// SigLIP's own calibration of text-to-image cosines: p = sigmoid(cos * SCALE + BIAS).
pub const LOGIT_SCALE: f32 = 112.668_91;
pub const LOGIT_BIAS: f32 = -16.771_725;

/// A text search keeps photos at least this close (cosine) to the description, and
/// within `TEXT_BELOW_BEST` of the best match: measured on real libraries, a photo that
/// shows what was asked scores 0.08 to 0.20, one that does not about 0.00 to 0.07.
pub const TEXT_FLOOR: f32 = 0.075;
pub const TEXT_BELOW_BEST: f32 = 0.06;
/// "Find similar": photos at least this close to the photo (the same scene is above
/// 0.9, the same kind of picture 0.7 to 0.8, an unrelated one about 0.5).
pub const LIKE_FLOOR: f32 = 0.6;
/// At most this many results.
pub const MAX_HITS: usize = 3000;

pub fn migrate(c: &Connection) -> Result<()> {
    c.execute_batch(
        r#"
        -- An image embedding of each photo (int8), of the file as it was (size, mtime,
        -- orientation), by the model the ML service named (meta 'visual_model'); `emb` NULL
        -- with an error when the photo could not be read.
        CREATE TABLE IF NOT EXISTS clip(
            photo_id INTEGER PRIMARY KEY REFERENCES photos(id) ON DELETE CASCADE,
            size INTEGER NOT NULL,
            mtime INTEGER NOT NULL,
            orientation INTEGER NOT NULL,
            emb BLOB,
            at INTEGER NOT NULL,
            error TEXT
        );
        "#,
    )?;
    Ok(())
}

/// Every embedding in memory.
#[derive(Default)]
pub struct Index {
    ids: Vec<i64>,
    vecs: Vec<i8>,
    norms: Vec<f32>,
    slot: HashMap<i64, usize>,
}

impl Index {
    pub fn load(c: &Connection) -> Result<Index> {
        let mut ix = Index::default();
        let mut st = c.prepare("SELECT photo_id, emb FROM clip WHERE emb IS NOT NULL")?;
        let mut rows = st.query([])?;
        while let Some(r) = rows.next()? {
            let id: i64 = r.get(0)?;
            let b = r.get_ref(1)?.as_blob()?;
            if b.len() == DIM {
                let v: Vec<i8> = b.iter().map(|&x| x as i8).collect();
                ix.upsert(id, &v);
            }
        }
        Ok(ix)
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    pub fn contains(&self, id: i64) -> bool {
        self.slot.contains_key(&id)
    }

    pub fn vector(&self, id: i64) -> Option<&[i8]> {
        self.slot.get(&id).map(|&s| &self.vecs[s * DIM..(s + 1) * DIM])
    }

    pub fn upsert(&mut self, id: i64, v: &[i8]) {
        debug_assert_eq!(v.len(), DIM);
        let n = norm(v).max(1e-6);
        match self.slot.get(&id) {
            Some(&s) => {
                self.vecs[s * DIM..(s + 1) * DIM].copy_from_slice(v);
                self.norms[s] = n;
            }
            None => {
                self.slot.insert(id, self.ids.len());
                self.ids.push(id);
                self.vecs.extend_from_slice(v);
                self.norms.push(n);
            }
        }
    }

    pub fn remove(&mut self, id: i64) {
        let Some(s) = self.slot.remove(&id) else { return };
        let last = self.ids.len() - 1;
        if s != last {
            let moved = self.ids[last];
            self.ids[s] = moved;
            self.norms[s] = self.norms[last];
            let (a, b) = self.vecs.split_at_mut(last * DIM);
            a[s * DIM..(s + 1) * DIM].copy_from_slice(&b[..DIM]);
            self.slot.insert(moved, s);
        }
        self.ids.pop();
        self.norms.pop();
        self.vecs.truncate(last * DIM);
    }

    /// Cosine of the query with every photo: the photos at least `floor` close (and
    /// within `below_best` of the best one, when given), best first, at most `k`.
    pub fn search(&self, q: &[i8], floor: f32, below_best: Option<f32>, k: usize) -> Vec<(i64, f32)> {
        use rayon::prelude::*;
        let qn = norm(q).max(1e-6);
        let mut hits: Vec<(i64, f32)> = self
            .ids
            .par_iter()
            .enumerate()
            .with_min_len(4096)
            .filter_map(|(s, &id)| {
                let c = dot(q, &self.vecs[s * DIM..(s + 1) * DIM]) as f32 / (qn * self.norms[s]);
                (c >= floor).then_some((id, c))
            })
            .collect();
        hits.sort_unstable_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        if let (Some(d), Some(best)) = (below_best, hits.first().map(|h| h.1)) {
            let cut = best - d;
            hits.retain(|h| h.1 >= cut);
        }
        hits.truncate(k);
        hits
    }

    /// Every photo's vector, for the duplicates report: (ids, vectors, norms).
    pub fn parts(&self) -> (&[i64], &[i8], &[f32]) {
        (&self.ids, &self.vecs, &self.norms)
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Status {
    /// An ML service is configured.
    pub enabled: bool,
    /// off, connecting, offline, unsupported (no search models), indexing, waiting, idle
    pub phase: String,
    pub error: Option<String>,
    /// Photos with an embedding, and photos shown in lists (outside the reject folders).
    pub done: i64,
    pub total: i64,
    /// Photos a minute, recently.
    pub rate: f64,
}

#[derive(Default)]
pub struct Ctl {
    /// Photos to embed before the rest (the ones on screen).
    pub priority: VecDeque<i64>,
    pub wake: bool,
}

pub struct Visual {
    pub status: RwLock<Status>,
    pub index: RwLock<Index>,
    pub ctl: Mutex<Ctl>,
    pub cv: Condvar,
    /// Text embeddings already asked for (a query typed again, sorting, paging).
    texts: Mutex<lru::Lru>,
}

mod lru {
    /// A tiny least-recently-used map for query embeddings.
    pub struct Lru {
        cap: usize,
        items: Vec<(String, Vec<i8>)>,
    }
    impl Lru {
        pub fn new(cap: usize) -> Lru {
            Lru { cap, items: Vec::new() }
        }
        pub fn get(&mut self, k: &str) -> Option<Vec<i8>> {
            let i = self.items.iter().position(|(key, _)| key == k)?;
            let item = self.items.remove(i);
            let v = item.1.clone();
            self.items.push(item);
            Some(v)
        }
        pub fn put(&mut self, k: String, v: Vec<i8>) {
            self.items.retain(|(key, _)| *key != k);
            self.items.push((k, v));
            if self.items.len() > self.cap {
                self.items.remove(0);
            }
        }
    }
}

impl Visual {
    pub fn new(cfg: &crate::config::Config) -> Visual {
        Visual {
            status: RwLock::new(Status { enabled: cfg.ml_url.is_some(), phase: "off".into(), ..Default::default() }),
            index: RwLock::new(Index::default()),
            ctl: Mutex::new(Ctl::default()),
            cv: Condvar::new(),
            texts: Mutex::new(lru::Lru::new(64)),
        }
    }

    pub fn wake(&self) {
        self.ctl.lock().wake = true;
        self.cv.notify_all();
    }

    /// Embed these photos next (the ones on screen, a folder being searched).
    pub fn prioritize(&self, ids: &[i64]) {
        let mut c = self.ctl.lock();
        for &id in ids.iter().rev() {
            c.priority.push_front(id);
        }
        c.priority.truncate(5000);
        c.wake = true;
        drop(c);
        self.cv.notify_all();
    }
}

/// The text query as SigLIP 2 was trained to match it with photos.
fn prompt(q: &str) -> String {
    let q = q.trim();
    let lower = q.to_lowercase();
    if lower.starts_with("a photo") || lower.starts_with("photo of") || lower.split_whitespace().count() > 8 {
        q.to_string()
    } else {
        format!("a photo of {q}.")
    }
}

/// Photos matching a description, best first, with their cosine.
pub fn search_text(state: &AppState, q: &str) -> Result<Vec<(i64, f32)>> {
    let q = q.trim();
    if q.is_empty() {
        return Ok(Vec::new());
    }
    let cached = state.visual.texts.lock().get(q);
    let v = match cached {
        Some(v) => v,
        None => {
            let url = state.cfg.ml_url.as_deref().ok_or_else(|| anyhow!("search by content needs the ML service (HYPERFOCAL_ML_URL)"))?;
            let client = crate::people::ml::Client::new(url, state.cfg.ml_token.clone());
            let e = client.embed_texts(&[prompt(q)]).map_err(|e| anyhow!("{e}"))?;
            let v = quantize(e.first().ok_or_else(|| anyhow!("no embedding for the text"))?);
            state.visual.texts.lock().put(q.to_string(), v.clone());
            v
        }
    };
    Ok(state.visual.index.read().search(&v, TEXT_FLOOR, Some(TEXT_BELOW_BEST), MAX_HITS))
}

/// Photos that look like this one, best first (the photo itself first).
pub fn search_like(state: &AppState, id: i64) -> Result<Vec<(i64, f32)>> {
    let ix = state.visual.index.read();
    let v = ix.vector(id).ok_or_else(|| anyhow!("this photo has not been looked at for search yet; try again in a while"))?.to_vec();
    Ok(ix.search(&v, LIKE_FLOOR, None, MAX_HITS))
}

/// The ranking of a search, for SQL: a temporary table of this connection
/// (`temp.vq`: id, rank), filled with the hits.
pub fn stage_hits(c: &Connection, hits: &[(i64, f32)]) -> Result<()> {
    c.execute_batch("CREATE TEMP TABLE IF NOT EXISTS vq(id INTEGER PRIMARY KEY, r REAL NOT NULL); DELETE FROM temp.vq;")?;
    let mut st = c.prepare_cached("INSERT OR IGNORE INTO temp.vq(id, r) VALUES(?1, ?2)")?;
    for &(id, r) in hits {
        st.execute(params![id, r as f64])?;
    }
    Ok(())
}

/// Stores an embedding of a photo as it is now (and puts it in the index).
pub fn store(state: &AppState, c: &Connection, rec: &crate::model::PhotoRec, emb: Option<&[f32]>, error: Option<&str>) -> Result<()> {
    let id = rec.id;
    let q = emb.map(quantize);
    c.execute(
        "INSERT INTO clip(photo_id, size, mtime, orientation, emb, at, error) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(photo_id) DO UPDATE SET size = excluded.size, mtime = excluded.mtime, orientation = excluded.orientation,
           emb = excluded.emb, at = excluded.at, error = excluded.error",
        params![id, rec.size, rec.mtime, rec.orientation, q.as_deref().map(to_blob), crate::db::now_ms(), error],
    )?;
    let mut ix = state.visual.index.write();
    match &q {
        Some(v) => ix.upsert(id, v),
        None => ix.remove(id),
    }
    Ok(())
}

/// For `/api/status`.
pub fn status_json(state: &AppState) -> serde_json::Value {
    let s = state.visual.status.read().clone();
    serde_json::json!({
        "enabled": s.enabled && s.phase != "unsupported",
        "phase": s.phase,
        "done": s.done,
        "total": s.total,
        "error": s.error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(seed: u64) -> Vec<f32> {
        let mut x = seed.wrapping_mul(0x9E3779B97F4A7C15) | 1;
        let mut v: Vec<f32> = (0..DIM)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x % 2000) as f32 / 1000.0 - 1.0
            })
            .collect();
        let n = v.iter().map(|a| a * a).sum::<f32>().sqrt();
        v.iter_mut().for_each(|a| *a /= n);
        v
    }

    #[test]
    fn search_ranks_by_cosine_and_cuts_below_the_best() {
        let mut ix = Index::default();
        let a = unit(1);
        let b = unit(2);
        // Photo 10 is a, 11 mostly a, 12 a bit of a, 13 unrelated.
        let mix = |w: f32| -> Vec<i8> { quantize(&a.iter().zip(&b).map(|(x, y)| w * x + (1.0 - w) * y).collect::<Vec<_>>()) };
        ix.upsert(10, &quantize(&a));
        ix.upsert(11, &mix(0.9));
        ix.upsert(12, &mix(0.3));
        ix.upsert(13, &quantize(&unit(3)));
        let hits = ix.search(&quantize(&a), 0.2, None, 10);
        assert_eq!(hits.iter().map(|h| h.0).collect::<Vec<_>>(), vec![10, 11, 12]);
        assert!(hits[0].1 > 0.99);
        let close = ix.search(&quantize(&a), 0.2, Some(0.2), 10);
        assert_eq!(close.iter().map(|h| h.0).collect::<Vec<_>>(), vec![10, 11]);
        // Removing keeps the others findable (the last one moves into the hole).
        ix.remove(10);
        assert_eq!(ix.len(), 3);
        assert!(ix.vector(13).is_some());
        assert_eq!(ix.search(&quantize(&a), 0.2, None, 10)[0].0, 11);
        ix.upsert(11, &quantize(&unit(4)));
        assert_eq!(ix.search(&quantize(&a), 0.2, None, 10).iter().map(|h| h.0).collect::<Vec<_>>(), vec![12]);
    }

    #[test]
    fn embeddings_come_back_from_the_index_database() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = crate::db::open(&tmp.path().join("i.db")).unwrap();
        let c = pool.get().unwrap();
        c.execute("INSERT INTO folders(id, parent_id, path, name, scanned_at) VALUES(1, NULL, '', 'Library', 1)", []).unwrap();
        let rec = crate::model::PhotoRec { folder_id: 1, name: "a.jpg".into(), stem: "a".into(), ext: "jpg".into(), kind: 2, ..Default::default() };
        let id = crate::repo::upsert(&c, &rec).unwrap();
        let v = quantize(&unit(9));
        c.execute("INSERT INTO clip(photo_id, size, mtime, orientation, emb, at) VALUES(?1, 0, 0, 1, ?2, 0)", params![id, to_blob(&v)]).unwrap();
        let ix = Index::load(&c).unwrap();
        assert_eq!(ix.len(), 1);
        assert_eq!(ix.vector(id), Some(&v[..]));
    }

    #[test]
    fn queries_become_captions() {
        assert_eq!(prompt(" dog on the beach "), "a photo of dog on the beach.");
        assert_eq!(prompt("A photo of a red car"), "A photo of a red car");
        let long = "two people walking along a long sandy beach at sunset with dogs";
        assert_eq!(prompt(long), long);
    }
}
