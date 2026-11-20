//! The people part of a photo's sidecar, both ways:
//! - out: confirmed, named faces and animals become `People|Name` / `Pets|Name`
//!   keywords, MWG regions (type Face or Pet) and IPTC "person shown" names, written in
//!   the background in batches you can undo from the history;
//! - in: named regions and people keywords other apps wrote (or Hyperfocal wrote before
//!   the index was lost) name the matching detections.

use super::engine::{Engine, subject_by_name};
use super::regions::{Rect, iou, rect_to_region, region_to_rect};
use super::{AUTO, CAT, CONFIRMED, DOG, FACE, IGNORED, PERSON, PET, SRC_XMP, keyword};
use crate::meta::regions::Regions;
use crate::ops::{Patch, PeopleXmp};
use crate::repo::Located;
use crate::state::AppState;
use anyhow::Result;
use rusqlite::{Connection, params};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Size of the stored image for `AppliedToDimensions`: the index's, else the analysed
/// image's (upright, so turned back when the orientation swaps the sides).
fn stored_dims(c: &Connection, loc: &Located) -> (f64, f64) {
    if let (Some(w), Some(h)) = (loc.rec.width, loc.rec.height)
        && w > 0
        && h > 0
    {
        return (w as f64, h as f64);
    }
    let (w, h): (f64, f64) = c
        .query_row("SELECT w, h FROM analysis WHERE photo_id = ?1", [loc.rec.id], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap_or((0.0, 0.0));
    if (5..=8).contains(&loc.rec.orientation) { (h, w) } else { (w, h) }
}

/// What the sidecar should say: keywords of the confirmed, named subjects, and every
/// face and pet region (theirs, plus regions read from the sidecar that no detection
/// took over).
pub fn desired(c: &Connection, loc: &Located) -> Result<(Vec<String>, PeopleXmp)> {
    let mut st = c.prepare_cached(
        "SELECT d.x, d.y, d.w, d.h, d.kind, s.name, s.kind, d.state, d.source FROM detections d LEFT JOIN subjects s ON s.id = d.subject_id
         WHERE d.photo_id = ?1 AND d.state != ?2 AND ((d.state = ?3 AND s.name IS NOT NULL) OR d.source = ?4)
         ORDER BY d.x",
    )?;
    let o = loc.rec.orientation;
    let mut keywords = Vec::new();
    let mut persons = Vec::new();
    let mut regions = Vec::new();
    let rows = st.query_map(params![loc.rec.id, IGNORED, CONFIRMED, SRC_XMP], |r| {
        Ok((
            Rect { x: r.get(0)?, y: r.get(1)?, w: r.get(2)?, h: r.get(3)? },
            r.get::<_, i64>(4)?,
            r.get::<_, Option<String>>(5)?,
            r.get::<_, Option<i64>>(6)?,
            r.get::<_, i64>(7)?,
        ))
    })?;
    for row in rows {
        let (rect, kind, name, skind, state) = row?;
        // A named subject's name only once it is confirmed.
        let name = name.filter(|_| state == CONFIRMED);
        if let (Some(n), Some(sk)) = (&name, skind) {
            let k = keyword(sk, n);
            if !keywords.contains(&k) {
                keywords.push(k);
            }
            if sk == PERSON && !persons.contains(n) {
                persons.push(n.clone());
            }
        }
        regions.push(rect_to_region(o, rect, name, if kind == FACE { "Face" } else { "Pet" }));
    }
    let (w, h) = stored_dims(c, loc);
    Ok((keywords, PeopleXmp { regions, w, h, persons, exact: false }))
}

/// The patch that brings a photo's sidecar in line with the index. `drop` lists names
/// (keywords) that may have to go: a person removed from the photo or renamed.
pub fn patch_for(c: &Connection, loc: &Located, drop: &HashSet<String>) -> Result<Option<Patch>> {
    let (keywords, people) = desired(c, loc)?;
    let has = |k: &str| loc.rec.hkeywords.iter().any(|h| h.eq_ignore_ascii_case(k));
    let add: Vec<String> = keywords.iter().filter(|k| !has(k)).cloned().collect();
    let remove: Vec<String> = drop.iter().filter(|d| has(d) && !keywords.iter().any(|k| k.eq_ignore_ascii_case(d))).cloned().collect();
    let cur_people: Vec<_> = loc.rec.regions.as_ref().map(|r| r.list.iter().filter(|g| g.is_people()).cloned().collect()).unwrap_or_default();
    let same_regions = cur_people.len() == people.regions.len()
        && cur_people.iter().zip(&people.regions).all(|(a, b)| {
            a.name == b.name && (a.x - b.x).abs() < 1e-4 && (a.y - b.y).abs() < 1e-4 && (a.w - b.w).abs() < 1e-4 && (a.h - b.h).abs() < 1e-4
        });
    if add.is_empty() && remove.is_empty() && same_regions && people.persons == loc.rec.persons {
        return Ok(None);
    }
    Ok(Some(Patch { keywords_add: add, keywords_remove: remove, people: Some(people), ..Default::default() }))
}

/// Photos waiting for their sidecar to be brought in line, with names that may have to
/// go. Written in batches by `writer`.
#[derive(Default)]
pub struct Pending {
    pub photos: HashMap<i64, HashSet<String>>,
}

impl super::People {
    /// Bring these photos' sidecars in line with the index (soon, in the background).
    pub fn write(&self, photos: impl IntoIterator<Item = (i64, Vec<String>)>) {
        let mut p = self.pending.lock();
        for (id, drop) in photos {
            p.photos.entry(id).or_default().extend(drop);
        }
        drop(p);
        self.write_wake.notify_one();
    }

    /// An undo restored sidecars: read their names back exactly.
    pub fn after_undo(&self, photos: &[i64]) {
        if photos.is_empty() {
            return;
        }
        let mut c = self.ctl.lock();
        c.exact.extend(photos.iter().copied());
        c.wake = true;
        drop(c);
        self.cv.notify_all();
    }
}

/// Writes pending sidecar changes, a batch at a time (each batch one entry in the
/// history), shortly after the last change so quick edits land together.
pub fn spawn_writer(state: Arc<AppState>) {
    tokio::spawn(async move {
        loop {
            state.people.write_wake.notified().await;
            tokio::time::sleep(std::time::Duration::from_millis(700)).await;
            let batch: HashMap<i64, HashSet<String>> = std::mem::take(&mut state.people.pending.lock().photos);
            if batch.is_empty() || !state.cfg.people_xmp || state.is_readonly() {
                continue;
            }
            let n = batch.len();
            let ids: Vec<i64> = batch.keys().copied().collect();
            let st = state.clone();
            let make = move |id: i64| -> Option<Patch> {
                let c = st.db.get().ok()?;
                let loc = crate::repo::load(&c, id).ok()??;
                let loc = fresh_people(&st, loc);
                let drop = batch.get(&id).cloned().unwrap_or_default();
                match patch_for(&c, &loc, &drop) {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::warn!("people: sidecar change for photo {id}: {e:#}");
                        None
                    }
                }
            };
            let label = if n == 1 { "Names of people and pets".to_string() } else { format!("Names of people and pets in {n} photos") };
            crate::ops::write_lazy(&state, label, ids, &make).await;
        }
    });
}

/// A photo record with its regions, person names and keywords read fresh from its
/// files (the index keeps no regions).
pub fn fresh_people(state: &AppState, mut loc: Located) -> Located {
    let dir = crate::scan::abs_dir(state, &loc.folder_path);
    let kind = crate::model::Kind::from_i32(loc.rec.kind);
    let fm = crate::images::raw::catch(|| crate::meta::read_file_meta(&dir.join(&loc.rec.name), kind))
        .unwrap_or(crate::meta::FileMeta { orientation: 1, ..Default::default() });
    let sc = loc.rec.sidecar.as_ref().and_then(|s| crate::meta::read_sidecar(&dir.join(s)));
    let orientation = loc.rec.orientation;
    crate::meta::merge_into(&mut loc.rec, &fm, sc.as_ref());
    // The index's orientation is the effective one already.
    loc.rec.orientation = orientation;
    loc
}

/// Take the names a photo's metadata gives into the index:
/// - a named face or pet region names the detection it covers (made confirmed), or is
///   kept as a detection of its own when nothing was detected there;
/// - a `People|Name` keyword with no region suggests that name for the photo's only
///   face (and `Pets|Name` for its only animal);
/// - `exact` (after an undo): confirmations the metadata no longer holds go back to
///   being suggestions, and regions it no longer has go.
///
/// Returns whether anything changed.
pub fn sync(c: &Connection, eng: &mut Engine, loc: &Located, exact: bool) -> Result<bool> {
    let photo = loc.rec.id;
    let o = loc.rec.orientation;
    struct Det {
        id: i64,
        kind: i64,
        rect: Rect,
        subject: Option<i64>,
        state: i64,
        source: i64,
    }
    let mut dets: Vec<Det> = {
        let mut st = c.prepare_cached("SELECT id, kind, x, y, w, h, subject_id, state, source FROM detections WHERE photo_id = ?1")?;
        st.query_map([photo], |r| {
            Ok(Det {
                id: r.get(0)?,
                kind: r.get(1)?,
                rect: Rect { x: r.get(2)?, y: r.get(3)?, w: r.get(4)?, h: r.get(5)? },
                subject: r.get(6)?,
                state: r.get(7)?,
                source: r.get(8)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?
    };
    let mut changed: Vec<i64> = Vec::new();
    let mut named: HashSet<i64> = HashSet::new();
    let mut used: HashSet<i64> = HashSet::new();
    let regions: Vec<_> = loc.rec.regions.as_ref().map(|r: &Regions| r.list.iter().filter(|g| g.is_people()).cloned().collect()).unwrap_or_default();
    let now = crate::db::now_ms();
    for g in &regions {
        let pet = g.kind.as_deref().is_some_and(|k| k.eq_ignore_ascii_case("pet"));
        let rect = region_to_rect(o, g);
        let best = dets
            .iter()
            .filter(|d| !used.contains(&d.id) && (d.kind == FACE) != pet)
            .map(|d| (d.id, iou(d.rect, rect)))
            .filter(|(_, v)| *v >= 0.3)
            .max_by(|a, b| a.1.total_cmp(&b.1));
        let name = g.name.as_deref().map(str::trim).filter(|n| !n.is_empty());
        match (best, name) {
            (Some((id, _)), Some(name)) => {
                used.insert(id);
                let d = dets.iter().find(|d| d.id == id).unwrap();
                let subject = subject_by_name(c, name, d.kind)?;
                named.insert(subject);
                if d.subject != Some(subject) || d.state != CONFIRMED {
                    c.execute("UPDATE detections SET subject_id = ?2, state = ?3, sim = NULL WHERE id = ?1", params![id, subject, CONFIRMED])?;
                    c.execute("DELETE FROM detection_rejects WHERE detection_id = ?1 AND subject_id = ?2", params![id, subject])?;
                    changed.push(id);
                }
            }
            (Some((id, _)), None) => {
                used.insert(id);
            }
            (None, name) => {
                // Kept as a detection of its own, so writing the sidecar keeps it.
                let kind = if !pet {
                    FACE
                } else {
                    name.and_then(|n| {
                        c.query_row("SELECT species FROM subjects WHERE kind = ?1 AND name = ?2 COLLATE NOCASE", params![PET, n], |r| r.get::<_, i64>(0)).ok()
                    })
                    .unwrap_or(DOG)
                };
                let subject = match name {
                    Some(n) => Some(subject_by_name(c, n, kind)?),
                    None => None,
                };
                if let Some(s) = subject {
                    named.insert(s);
                }
                let id: i64 = c.query_row(
                    "INSERT INTO detections(photo_id, kind, x, y, w, h, score, quality, emb, subject_id, state, source, created)
                     VALUES(?1, ?2, ?3, ?4, ?5, ?6, 1.0, 0.0, NULL, ?7, ?8, ?9, ?10) RETURNING id",
                    params![photo, kind, rect.x, rect.y, rect.w, rect.h, subject, if subject.is_some() { CONFIRMED } else { AUTO }, SRC_XMP, now],
                    |r| r.get(0),
                )?;
                used.insert(id);
                changed.push(id);
                dets.push(Det { id, kind, rect, subject, state: CONFIRMED, source: SRC_XMP });
            }
        }
    }
    // People keywords without a region: a suggestion for the photo's only face (or pet).
    for kw in &loc.rec.hkeywords {
        let (skind, name) = match kw.split_once('|') {
            Some((root, rest)) if root.eq_ignore_ascii_case("People") && !rest.contains('|') => (PERSON, rest),
            Some((root, rest)) if root.eq_ignore_ascii_case("Pets") && !rest.contains('|') => (PET, rest),
            _ => continue,
        };
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        let candidates: Vec<&Det> = dets.iter().filter(|d| d.state != IGNORED && ((d.kind == FACE) == (skind == PERSON))).collect();
        let existing: Option<i64> =
            c.query_row("SELECT id FROM subjects WHERE kind = ?1 AND name = ?2 COLLATE NOCASE", params![skind, name], |r| r.get(0)).ok();
        if let Some(s) = existing
            && candidates.iter().any(|d| d.subject == Some(s))
        {
            named.insert(s);
            continue;
        }
        if candidates.len() != 1 {
            continue;
        }
        let d = candidates[0];
        if d.state == CONFIRMED || (d.subject.is_some() && existing.is_some_and(|s| d.subject != Some(s)) && d.state == CONFIRMED) {
            continue;
        }
        let subject = subject_by_name(c, name, if skind == PERSON { FACE } else if d.kind == CAT { CAT } else { DOG })?;
        named.insert(subject);
        if d.subject != Some(subject) {
            c.execute("UPDATE detections SET subject_id = ?2, state = ?3, sim = NULL WHERE id = ?1", params![d.id, subject, AUTO])?;
            changed.push(d.id);
        }
    }
    if exact {
        let keywords: HashSet<String> = loc.rec.hkeywords.iter().map(|k| k.to_lowercase()).collect();
        for d in &dets {
            if used.contains(&d.id) {
                continue;
            }
            if d.source == SRC_XMP {
                c.execute("DELETE FROM detections WHERE id = ?1", [d.id])?;
                changed.push(d.id);
                continue;
            }
            if d.state != CONFIRMED {
                continue;
            }
            let Some(s) = d.subject else { continue };
            let (sk, name): (i64, Option<String>) = c.query_row("SELECT kind, name FROM subjects WHERE id = ?1", [s], |r| Ok((r.get(0)?, r.get(1)?)))?;
            let Some(name) = name else { continue };
            if !keywords.contains(&keyword(sk, &name).to_lowercase()) {
                c.execute("UPDATE detections SET state = ?2 WHERE id = ?1", params![d.id, AUTO])?;
                changed.push(d.id);
            }
        }
    }
    if !changed.is_empty() {
        eng.refresh(c, &changed)?;
    }
    let _ = named;
    Ok(!changed.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meta::regions::Region;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("PRAGMA foreign_keys=ON; CREATE TABLE photos(id INTEGER PRIMARY KEY);").unwrap();
        crate::people::migrate(&c).unwrap();
        c.execute("INSERT INTO photos(id) VALUES(1)", []).unwrap();
        c
    }

    fn det(c: &Connection, kind: i64, r: Rect) -> i64 {
        c.query_row(
            "INSERT INTO detections(photo_id, kind, x, y, w, h, score, quality, created) VALUES(1, ?1, ?2, ?3, ?4, ?5, 0.9, 0.8, 0) RETURNING id",
            params![kind, r.x, r.y, r.w, r.h],
            |r| r.get(0),
        )
        .unwrap()
    }

    fn loc(o: i32, regions: Vec<Region>, hk: Vec<&str>) -> Located {
        let mut rec = crate::model::PhotoRec { id: 1, orientation: o, width: Some(6000), height: Some(4000), ..Default::default() };
        rec.regions = Some(Regions { w: 6000.0, h: 4000.0, list: regions });
        rec.hkeywords = hk.into_iter().map(String::from).collect();
        Located { rec, folder_path: String::new(), folder_is_reject: false, folder_parent: None }
    }

    #[test]
    fn regions_name_detections_and_round_trip() {
        let c = setup();
        let mut e = Engine::empty();
        // Portrait photo (orientation 6): two faces on the upright image.
        let a = det(&c, FACE, Rect { x: 0.2, y: 0.3, w: 0.1, h: 0.08 });
        let b = det(&c, FACE, Rect { x: 0.6, y: 0.3, w: 0.1, h: 0.08 });
        let pet = det(&c, DOG, Rect { x: 0.1, y: 0.6, w: 0.5, h: 0.3 });
        let o = 6;
        // The sidecar (stored orientation) names the first face and the dog.
        let regions = vec![
            rect_to_region(o, Rect { x: 0.21, y: 0.3, w: 0.1, h: 0.08 }, Some("Ann".into()), "Face"),
            rect_to_region(o, Rect { x: 0.1, y: 0.62, w: 0.5, h: 0.3 }, Some("Rex".into()), "Pet"),
            // A face nothing was detected at.
            rect_to_region(o, Rect { x: 0.8, y: 0.8, w: 0.05, h: 0.05 }, Some("Bo".into()), "Face"),
        ];
        let l = loc(o, regions, vec![]);
        assert!(sync(&c, &mut e, &l, false).unwrap());
        let who = |id: i64| -> (Option<String>, i64) {
            c.query_row("SELECT s.name, d.state FROM detections d LEFT JOIN subjects s ON s.id = d.subject_id WHERE d.id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?))).unwrap()
        };
        assert_eq!(who(a), (Some("Ann".into()), CONFIRMED));
        assert_eq!(who(b), (None, AUTO));
        assert_eq!(who(pet), (Some("Rex".into()), CONFIRMED));
        let n: i64 = c.query_row("SELECT count(*) FROM detections WHERE source = ?1", [SRC_XMP], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        // Again: nothing changes.
        assert!(!sync(&c, &mut e, &l, false).unwrap());
        // What the sidecar should say now: the same three regions, back in stored coordinates.
        let (kw, people) = desired(&c, &l).unwrap();
        assert_eq!(kw.len(), 3);
        assert!(kw.contains(&"People|Ann".to_string()) && kw.contains(&"Pets|Rex".to_string()) && kw.contains(&"People|Bo".to_string()));
        assert_eq!(people.persons.len(), 2);
        assert_eq!(people.regions.len(), 3);
        assert_eq!((people.w, people.h), (6000.0, 4000.0));
        let ann = people.regions.iter().find(|r| r.name.as_deref() == Some("Ann")).unwrap();
        let back = region_to_rect(o, ann);
        assert!((back.x - 0.2).abs() < 1e-9 && (back.y - 0.3).abs() < 1e-9);
        // No change needed when the sidecar already says all that.
        let mut l2 = l.clone();
        l2.rec.regions = Some(Regions { w: 6000.0, h: 4000.0, list: people.regions.clone() });
        l2.rec.hkeywords = kw.clone();
        l2.rec.persons = people.persons.clone();
        assert!(patch_for(&c, &l2, &HashSet::new()).unwrap().is_none());
        // An undo removed Ann from the sidecar: exact sync takes her back to a suggestion.
        let l3 = loc(o, vec![], vec!["Pets|Rex", "People|Bo"]);
        sync(&c, &mut e, &l3, true).unwrap();
        assert_eq!(who(a), (Some("Ann".into()), AUTO));
        assert_eq!(who(pet).1, CONFIRMED, "Rex keeps his keyword");
    }

    #[test]
    fn keyword_suggests_the_only_face() {
        let c = setup();
        let mut e = Engine::empty();
        let a = det(&c, FACE, Rect { x: 0.4, y: 0.3, w: 0.1, h: 0.1 });
        sync(&c, &mut e, &loc(1, vec![], vec!["People|Cleo", "Places|Lisbon"]), false).unwrap();
        let (name, state): (String, i64) =
            c.query_row("SELECT s.name, d.state FROM detections d JOIN subjects s ON s.id = d.subject_id WHERE d.id = ?1", [a], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((name.as_str(), state), ("Cleo", AUTO));
    }
}
