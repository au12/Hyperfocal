//! Who is who: every new face (or animal) is matched against the faces already known,
//! and joins a person, a pet or an unnamed group, or starts a group with the faces it
//! resembles.
//!
//! How alike two faces must be depends on the poorer of the two. The ML service rates
//! each face (its embedding's length before normalization: masks, profiles, blur and
//! veils make short embeddings), and a poor face must be much closer to count. Measured
//! on two weddings' worth of photos (AuraFace): two clear frontal faces of one person
//! sit at 0.45 to 0.85, different people below 0.35, and the mistakes above that all
//! involved a masked, blurred or turned-away face; hence 0.38 for two good faces,
//! rising to 0.64 when either is poor.
//!
//! A face is matched by votes of its close neighbours: confirmed faces count more than
//! automatic ones, a clear winner is needed (a near tie between two people is left for
//! you), a person can be in a photo only once, and a "not this person" is never
//! overruled. A face with no named or grouped neighbour forms a new unnamed group with
//! the ungrouped faces close to it (one per photo).

use super::index::{Hit, Space, from_blob};
use super::{AUTO, CONFIRMED, DOG, FACE, IGNORED, PERSON, PET};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::{HashMap, HashSet};

/// Least similarity two detections of `kind` need to be the same one, given their
/// qualities (0..1).
pub fn link(kind: i64, qa: f32, qb: f32) -> f32 {
    let q = qa.min(qb).clamp(0.0, 1.0);
    if kind == FACE { 0.38 + 0.26 * (1.0 - q) } else { 0.52 + 0.12 * (1.0 - q) }
}

fn floor(kind: i64) -> f32 {
    link(kind, 1.0, 1.0) - 0.01
}

/// Whether a detection is good enough to be matched automatically (a face too poor to
/// recognise, an animal too small).
pub fn usable(_kind: i64, quality: f64) -> bool {
    quality >= 0.05
}

/// The embedding spaces, by a detection kind that stands for each: faces, and animals.
/// Dogs and cats share one space: they share the pet model, and the detector sometimes
/// takes one for the other, which must not keep a pet from its look-alikes.
const SPACES: [i64; 2] = [FACE, DOG];

/// Quality of an animal: how big it is (pixels of its short side in the analysed image)
/// and how sure the detector is.
pub fn pet_quality(px: f32, score: f32) -> f32 {
    ((px - 40.0) / 216.0).clamp(0.0, 1.0).sqrt() * ((score - 0.3) / 0.5).clamp(0.0, 1.0).sqrt()
}

pub struct Engine {
    pub faces: Space,
    /// Dogs and cats.
    pub pets: Space,
    /// Detection -> subjects it was said not to be.
    rejects: HashMap<i64, Vec<i64>>,
}

/// One change made by `assign`, for the caller's events.
#[derive(Debug, Default)]
pub struct Assigned {
    pub photos: HashSet<i64>,
    pub new_groups: usize,
}

impl Engine {
    pub fn empty() -> Engine {
        Engine { faces: Space::default(), pets: Space::default(), rejects: HashMap::new() }
    }

    /// Load every usable embedding from the index.
    pub fn load(c: &Connection) -> Result<Engine> {
        let mut e = Engine::empty();
        let mut st = c.prepare("SELECT id, photo_id, kind, quality, emb, subject_id, state FROM detections WHERE emb IS NOT NULL AND state != ?1")?;
        let mut rows = st.query([IGNORED])?;
        while let Some(r) = rows.next()? {
            let (id, photo, kind, q): (i64, i64, i64, f64) = (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?);
            if !usable(kind, q) {
                continue;
            }
            let Some(v) = from_blob(r.get_ref(4)?.as_blob()?) else { continue };
            let subject: Option<i64> = r.get(5)?;
            let state: i64 = r.get(6)?;
            e.space(kind).insert(id, photo, &v, q as f32, subject.unwrap_or(0), state == CONFIRMED);
        }
        let mut st = c.prepare("SELECT detection_id, subject_id FROM detection_rejects")?;
        for r in st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))? {
            let (d, s) = r?;
            e.rejects.entry(d).or_default().push(s);
        }
        Ok(e)
    }

    pub fn space(&mut self, kind: i64) -> &mut Space {
        if kind == FACE { &mut self.faces } else { &mut self.pets }
    }

    pub fn space_ref(&self, kind: i64) -> &Space {
        if kind == FACE { &self.faces } else { &self.pets }
    }

    /// Faces, animals.
    pub fn counts(&self) -> (usize, usize) {
        (self.faces.len(), self.pets.len())
    }

    /// Put a detection in the index (or update it), if it is usable.
    #[allow(clippy::too_many_arguments)]
    pub fn add(&mut self, id: i64, photo: i64, kind: i64, quality: f64, emb: &[i8], subject: Option<i64>, state: i64) {
        if state == IGNORED || !usable(kind, quality) {
            self.remove(id);
            return;
        }
        self.space(kind).insert(id, photo, emb, quality as f32, subject.unwrap_or(0), state == CONFIRMED);
    }

    pub fn remove(&mut self, id: i64) {
        self.faces.remove(id);
        self.pets.remove(id);
    }

    /// Detections deleted from the index (with their photo, or by a new analysis).
    pub fn forget(&mut self, ids: &[i64]) {
        for &id in ids {
            self.remove(id);
            self.rejects.remove(&id);
        }
    }

    pub fn set_label(&mut self, id: i64, subject: Option<i64>, state: i64) {
        if state == IGNORED {
            self.remove(id);
            return;
        }
        for s in [&mut self.faces, &mut self.pets] {
            s.set_label(id, subject.unwrap_or(0), state == CONFIRMED);
        }
    }

    /// Re-read the labels of these detections from the index after a change.
    pub fn refresh(&mut self, c: &Connection, ids: &[i64]) -> Result<()> {
        for chunk in ids.chunks(500) {
            let sql = format!(
                "SELECT id, photo_id, kind, quality, emb, subject_id, state FROM detections WHERE id IN ({})",
                chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",")
            );
            let mut st = c.prepare(&sql)?;
            let mut rows = st.query(rusqlite::params_from_iter(chunk.iter()))?;
            let mut seen = HashSet::new();
            while let Some(r) = rows.next()? {
                let id: i64 = r.get(0)?;
                seen.insert(id);
                let emb = r.get_ref(4)?.as_blob_or_null()?.and_then(from_blob);
                let (photo, kind, q, subject, state): (i64, i64, f64, Option<i64>, i64) = (r.get(1)?, r.get(2)?, r.get(3)?, r.get(5)?, r.get(6)?);
                match emb {
                    Some(v) => self.add(id, photo, kind, q, &v, subject, state),
                    None => self.remove(id),
                }
            }
            for id in chunk {
                if !seen.contains(id) {
                    self.remove(*id);
                    self.rejects.remove(id);
                }
            }
        }
        Ok(())
    }

    pub fn relabel_subject(&mut self, from: i64, to: Option<i64>) {
        for s in [&mut self.faces, &mut self.pets] {
            s.relabel_subject(from, to.unwrap_or(0));
        }
    }

    pub fn add_reject(&mut self, detection: i64, subject: i64) {
        let v = self.rejects.entry(detection).or_default();
        if !v.contains(&subject) {
            v.push(subject);
        }
    }

    /// Subjects merged into another: their rejects move along.
    pub fn move_rejects(&mut self, from: i64, to: i64) {
        for v in self.rejects.values_mut() {
            for s in v.iter_mut() {
                if *s == from {
                    *s = to;
                }
            }
            v.dedup();
        }
    }

    /// Match the given detections (already in the index, not yet assigned), best first.
    /// Their neighbours are found in one pass over each index.
    pub fn assign(&mut self, c: &Connection, ids: &[i64]) -> Result<Assigned> {
        let mut out = Assigned::default();
        let mut order: Vec<(i64, i64, f32)> = Vec::new();
        for &id in ids {
            for kind in SPACES {
                let sp = self.space_ref(kind);
                if let Some(s) = sp.slot(id)
                    && sp.subject(s) == 0
                {
                    order.push((id, kind, sp.quality(s)));
                }
            }
        }
        order.sort_by(|a, b| b.2.total_cmp(&a.2).then(a.0.cmp(&b.0)));
        order.dedup_by_key(|o| o.0);
        let mut hits = self.neighbours(order.iter().map(|o| (o.0, o.1)), 64);
        for (id, kind, _) in order {
            if let Some(h) = hits.remove(&id) {
                self.assign_hits(c, id, kind, &h, &mut out)?;
            }
        }
        Ok(out)
    }

    /// The neighbours (similarity of at least the lowest threshold, best first) of these
    /// detections, in one pass over each index.
    fn neighbours(&self, items: impl Iterator<Item = (i64, i64)>, k: usize) -> HashMap<i64, Vec<Hit>> {
        let items: Vec<(i64, i64)> = items.collect();
        let mut out = HashMap::new();
        for kind in SPACES {
            let sp = self.space_ref(kind);
            let ids: Vec<(i64, usize)> = items.iter().filter(|i| i.1 == kind).filter_map(|i| sp.slot(i.0).map(|s| (i.0, s))).collect();
            if ids.is_empty() {
                continue;
            }
            let qs: Vec<&[i8]> = ids.iter().map(|&(_, s)| sp.vector(s)).collect();
            for ((id, _), h) in ids.iter().zip(sp.search_many(&qs, floor(kind), k)) {
                out.insert(*id, h);
            }
        }
        out
    }

    /// Match one detection, given its neighbours.
    fn assign_hits(&mut self, c: &Connection, id: i64, kind: i64, hits: &[Hit], out: &mut Assigned) -> Result<()> {
        let sp = self.space_ref(kind);
        let Some(slot) = sp.slot(id) else { return Ok(()) };
        if sp.subject(slot) != 0 {
            return Ok(());
        }
        let photo = sp.photo(slot);
        let qa = sp.quality(slot);
        let rejected: HashSet<i64> = self.rejects.get(&id).map(|v| v.iter().copied().collect()).unwrap_or_default();
        // A person (or pet) is in a photo once: subjects other detections of this photo
        // have are out of the question.
        let taken: HashSet<i64> = {
            let mut st = c.prepare_cached("SELECT subject_id FROM detections WHERE photo_id = ?1 AND id != ?2 AND subject_id IS NOT NULL AND state != ?3")?;
            st.query_map(params![photo, id, IGNORED], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?
        };
        let mut votes: HashMap<i64, (f32, f32)> = HashMap::new();
        let mut loose: Vec<(usize, f32)> = Vec::new();
        for h in hits {
            if h.slot == slot {
                continue;
            }
            let thr = link(kind, qa, sp.quality(h.slot));
            if h.sim < thr {
                continue;
            }
            let s = sp.subject(h.slot);
            if s == 0 {
                if sp.photo(h.slot) != photo {
                    loose.push((h.slot, h.sim));
                }
                continue;
            }
            if rejected.contains(&s) || taken.contains(&s) {
                continue;
            }
            let w = (h.sim - thr + 0.05) * if sp.confirmed(h.slot) { 1.5 } else { 1.0 };
            let e = votes.entry(s).or_insert((0.0, 0.0));
            e.0 += w;
            e.1 = e.1.max(h.sim);
        }
        let mut ranked: Vec<(i64, f32, f32)> = votes.into_iter().map(|(s, (v, best))| (s, v, best)).collect();
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        if let Some(&(s, v, best)) = ranked.first() {
            // A near tie between two subjects (often one person in two groups) is left
            // for you to settle.
            if ranked.get(1).is_none_or(|r| r.1 < 0.6 * v) {
                c.prepare_cached("UPDATE detections SET subject_id = ?2, state = ?3, sim = ?4 WHERE id = ?1")?.execute(params![id, s, AUTO, best as f64])?;
                self.space(kind).set_label(id, s, false);
                out.photos.insert(photo);
            }
            return Ok(());
        }
        if loose.is_empty() {
            return Ok(());
        }
        // A new group: this detection and its ungrouped look-alikes, one per photo.
        let mut per_photo: HashMap<i64, (usize, f32)> = HashMap::new();
        for (s, sim) in loose {
            let p = sp.photo(s);
            if per_photo.get(&p).is_none_or(|x| sim > x.1) {
                per_photo.insert(p, (s, sim));
            }
        }
        let members: Vec<(i64, i64, f32)> = per_photo.into_values().map(|(s, sim)| (sp.id(s), sp.photo(s), sim)).collect();
        let now = crate::db::now_ms();
        // A group of animals is of the species its first one is.
        let species: i64 = if kind == FACE { FACE } else { c.query_row("SELECT kind FROM detections WHERE id = ?1", [id], |r| r.get(0))? };
        let subject: i64 = c.query_row(
            "INSERT INTO subjects(kind, species, name, hidden, created, updated) VALUES(?1, ?2, NULL, 0, ?3, ?3) RETURNING id",
            params![if kind == FACE { PERSON } else { PET }, species, now],
            |r| r.get(0),
        )?;
        out.new_groups += 1;
        let mut upd = c.prepare_cached("UPDATE detections SET subject_id = ?2, state = ?3, sim = ?4 WHERE id = ?1 AND subject_id IS NULL")?;
        upd.execute(params![id, subject, AUTO, Option::<f64>::None])?;
        out.photos.insert(photo);
        let mut labels = vec![id];
        for (mid, mphoto, sim) in members {
            if upd.execute(params![mid, subject, AUTO, sim as f64])? > 0 {
                labels.push(mid);
                out.photos.insert(mphoto);
            }
        }
        drop(upd);
        for l in labels {
            self.space(kind).set_label(l, subject, false);
        }
        Ok(())
    }

    /// Faces just confirmed for a subject may decide faces that had no match: those
    /// close to them are matched again. (From the best 512 of them when there are more:
    /// plenty to find the look-alikes, and an edit never waits long.)
    pub fn propagate(&mut self, c: &Connection, ids: &[i64]) -> Result<Assigned> {
        let mut from: Vec<(f32, i64, i64)> = Vec::new();
        for &id in ids {
            for kind in SPACES {
                let sp = self.space_ref(kind);
                if let Some(s) = sp.slot(id) {
                    from.push((sp.quality(s), id, kind));
                }
            }
        }
        from.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
        from.truncate(512);
        let hits = self.neighbours(from.iter().map(|f| (f.1, f.2)), 256);
        let mut cand: Vec<i64> = Vec::new();
        for (qa, id, kind) in from {
            let sp = self.space_ref(kind);
            for h in hits.get(&id).into_iter().flatten() {
                if sp.subject(h.slot) == 0 && h.sim >= link(kind, qa, sp.quality(h.slot)) {
                    cand.push(sp.id(h.slot));
                }
            }
        }
        cand.sort_unstable();
        cand.dedup();
        self.assign(c, &cand)
    }
}

/// Form the unnamed groups and the suggestions again from scratch, best faces first
/// (they make the best seeds). Named people and pets, their confirmed faces and hidden
/// groups stay as they are. `progress` gets 0..1.
///
/// The faces go in chunks: the neighbours of a chunk are found in one pass over the
/// index (`search_many`), then the chunk is matched in order. The engine is locked one
/// chunk at a time, so edits made meanwhile wait a moment at most.
pub fn regroup(lock: &parking_lot::Mutex<Option<Engine>>, conn: &mut Connection, progress: &dyn Fn(f64)) -> Result<Assigned> {
    let todo: Vec<(i64, i64)> = {
        let mut g = lock.lock();
        let eng = g.get_or_insert_with(Engine::empty);
        let tx = conn.transaction()?;
        tx.execute("UPDATE detections SET subject_id = NULL, sim = NULL WHERE state = ?1 AND subject_id IS NOT NULL", [AUTO])?;
        tx.execute(
            "DELETE FROM subjects WHERE name IS NULL AND hidden = 0 AND NOT EXISTS (SELECT 1 FROM detections d WHERE d.subject_id = subjects.id)",
            [],
        )?;
        tx.commit()?;
        for s in [&mut eng.faces, &mut eng.pets] {
            let ids: Vec<i64> = (0..s.len()).filter(|&i| !s.confirmed(i)).map(|i| s.id(i)).collect();
            for id in ids {
                s.set_label(id, 0, false);
            }
        }
        // Subjects that only kept confirmed faces keep those labels; the rest is free.
        let mut todo: Vec<(f32, i64, i64)> = Vec::new();
        for kind in SPACES {
            let s = eng.space_ref(kind);
            todo.extend((0..s.len()).filter(|&i| s.subject(i) == 0).map(|i| (s.quality(i), s.id(i), kind)));
        }
        todo.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
        todo.into_iter().map(|(_, id, kind)| (id, kind)).collect()
    };
    let total = todo.len().max(1);
    let mut out = Assigned::default();
    const CHUNK: usize = 256;
    for (n, chunk) in todo.chunks(CHUNK).enumerate() {
        let mut g = lock.lock();
        let eng = g.get_or_insert_with(Engine::empty);
        let mut hits: HashMap<i64, Vec<Hit>> = HashMap::new();
        for kind in SPACES {
            let sp = eng.space_ref(kind);
            let ids: Vec<i64> = chunk.iter().filter(|c| c.1 == kind && sp.contains(c.0)).map(|c| c.0).collect();
            let qs: Vec<&[i8]> = ids.iter().filter_map(|&id| sp.slot(id)).map(|s| sp.vector(s)).collect();
            for (id, h) in ids.iter().zip(sp.search_many(&qs, floor(kind), 64)) {
                hits.insert(*id, h);
            }
        }
        let tx = conn.transaction()?;
        for &(id, kind) in chunk {
            if let Some(h) = hits.remove(&id) {
                eng.assign_hits(&tx, id, kind, &h, &mut out)?;
            }
        }
        tx.commit()?;
        drop(g);
        progress(((n + 1) * CHUNK).min(total) as f64 / total as f64);
    }
    Ok(out)
}

/// The subject a name belongs to (for this kind of detection), made when new.
pub fn subject_by_name(c: &Connection, name: &str, det_kind: i64) -> Result<i64> {
    let (kind, species) = if det_kind == FACE { (PERSON, FACE) } else { (PET, det_kind) };
    if let Some(id) = c
        .query_row("SELECT id FROM subjects WHERE kind = ?1 AND name = ?2 COLLATE NOCASE", params![kind, name], |r| r.get::<_, i64>(0))
        .optional()?
    {
        return Ok(id);
    }
    let now = crate::db::now_ms();
    Ok(c.query_row(
        "INSERT INTO subjects(kind, species, name, hidden, created, updated) VALUES(?1, ?2, ?3, 0, ?4, ?4) RETURNING id",
        params![kind, species, name, now],
        |r| r.get(0),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::people::index::quantize;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("PRAGMA foreign_keys=ON; CREATE TABLE photos(id INTEGER PRIMARY KEY);").unwrap();
        crate::people::migrate(&c).unwrap();
        c
    }

    /// A unit vector near `base` (a person), offset by a little of `noise`.
    fn face(base: u64, noise: u64, amount: f32) -> Vec<f32> {
        let make = |seed: u64| -> Vec<f32> {
            let mut x = seed.wrapping_mul(0x9E3779B97F4A7C15) | 1;
            (0..super::super::DIM)
                .map(|_| {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    (x % 2000) as f32 / 1000.0 - 1.0
                })
                .collect()
        };
        let (b, n) = (make(base), make(noise + 1000));
        let mut v: Vec<f32> = b.iter().zip(&n).map(|(x, y)| x + amount * y).collect();
        let l = v.iter().map(|a| a * a).sum::<f32>().sqrt();
        v.iter_mut().for_each(|a| *a /= l);
        v
    }

    fn add(c: &Connection, e: &mut Engine, photo: i64, v: &[f32], q: f64) -> i64 {
        c.execute("INSERT OR IGNORE INTO photos(id) VALUES(?1)", [photo]).unwrap();
        let emb = quantize(v);
        let id: i64 = c
            .query_row(
                "INSERT INTO detections(photo_id, kind, x, y, w, h, score, quality, emb, created) VALUES(?1, 1, 0, 0, 0.1, 0.1, 0.9, ?2, ?3, 0) RETURNING id",
                params![photo, q, crate::people::index::to_blob(&emb)],
                |r| r.get(0),
            )
            .unwrap();
        e.add(id, photo, FACE, q, &emb, None, AUTO);
        id
    }

    fn subject_of(c: &Connection, id: i64) -> Option<i64> {
        c.query_row("SELECT subject_id FROM detections WHERE id = ?1", [id], |r| r.get(0)).unwrap()
    }

    #[test]
    fn groups_and_matches() {
        let c = db();
        let mut e = Engine::empty();
        // Person A in photos 1-3, person B in photos 1 and 4: similar within, unlike across.
        let a1 = add(&c, &mut e, 1, &face(1, 1, 0.5), 1.0);
        let b1 = add(&c, &mut e, 1, &face(2, 2, 0.5), 1.0);
        e.assign(&c, &[a1, b1]).unwrap();
        assert_eq!(subject_of(&c, a1), None, "nothing to group with yet");
        let a2 = add(&c, &mut e, 2, &face(1, 3, 0.5), 1.0);
        let r = e.assign(&c, &[a2]).unwrap();
        assert_eq!(r.new_groups, 1);
        let ga = subject_of(&c, a2).unwrap();
        assert_eq!(subject_of(&c, a1), Some(ga));
        assert_eq!(subject_of(&c, b1), None);
        let a3 = add(&c, &mut e, 3, &face(1, 4, 0.5), 1.0);
        let b4 = add(&c, &mut e, 4, &face(2, 5, 0.5), 1.0);
        e.assign(&c, &[a3, b4]).unwrap();
        assert_eq!(subject_of(&c, a3), Some(ga));
        let gb = subject_of(&c, b4).unwrap();
        assert_ne!(gb, ga);
        assert_eq!(subject_of(&c, b1), Some(gb));
        // A poor view of A (low quality) needs a much closer match: this one is too far.
        let poor = add(&c, &mut e, 5, &face(1, 6, 1.4), 0.1);
        e.assign(&c, &[poor]).unwrap();
        assert_eq!(subject_of(&c, poor), None);
        // "Not A": never suggested again, even when it looks like A.
        let a6 = add(&c, &mut e, 6, &face(1, 7, 0.5), 1.0);
        c.execute("INSERT INTO detection_rejects VALUES(?1, ?2)", params![a6, ga]).unwrap();
        e.add_reject(a6, ga);
        e.assign(&c, &[a6]).unwrap();
        assert_ne!(subject_of(&c, a6), Some(ga));
        // Once in a photo: a second face like A in photo 3 does not join A.
        let twin = add(&c, &mut e, 3, &face(1, 8, 0.5), 1.0);
        e.assign(&c, &[twin]).unwrap();
        assert_ne!(subject_of(&c, twin), Some(ga));
    }

    #[test]
    fn regroup_keeps_confirmed() {
        let mut c = db();
        let mut e = Engine::empty();
        let ids: Vec<i64> = (0..6).map(|i| add(&c, &mut e, 10 + i, &face(7, 20 + i as u64, 0.5), 1.0)).collect();
        e.assign(&c, &ids).unwrap();
        let g = subject_of(&c, ids[0]).unwrap();
        assert!(ids.iter().all(|&i| subject_of(&c, i) == Some(g)));
        // Name the group and confirm two of its faces; regroup.
        c.execute("UPDATE subjects SET name = 'Ann' WHERE id = ?1", [g]).unwrap();
        c.execute("UPDATE detections SET state = 1 WHERE id IN (?1, ?2)", params![ids[0], ids[1]]).unwrap();
        e.refresh(&c, &ids).unwrap();
        let lock = parking_lot::Mutex::new(Some(e));
        regroup(&lock, &mut c, &|_| {}).unwrap();
        // The confirmed ones stay; the others come back as suggestions for Ann.
        assert!(ids.iter().all(|&i| subject_of(&c, i) == Some(g)));
        let states: Vec<i64> = ids.iter().map(|&i| c.query_row("SELECT state FROM detections WHERE id = ?1", [i], |r| r.get(0)).unwrap()).collect();
        assert_eq!(states, vec![1, 1, 0, 0, 0, 0]);
    }

    #[test]
    fn thresholds() {
        assert!((link(FACE, 1.0, 1.0) - 0.38).abs() < 1e-6);
        assert!((link(FACE, 0.0, 1.0) - 0.64).abs() < 1e-6);
        assert!(link(DOG, 1.0, 1.0) > link(FACE, 1.0, 1.0));
        assert!(pet_quality(300.0, 0.9) > 0.9);
        assert!(pet_quality(40.0, 0.9) < 0.01);
    }
}
