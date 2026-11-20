//! Changes to the library: XMP sidecar writes (through ExifTool) and reject/restore
//! moves. Every change is serialised by the write lock, backed up and journaled.

use crate::files;
use crate::meta::{self, xmp::Xmp};
use crate::model::{Kind, PhotoRec};
use crate::repo::{self, Located};
use crate::scan;
use crate::sets::Plan;
use crate::state::{AppState, Event};
use anyhow::{Context, Result, anyhow, bail};
use rusqlite::params;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn double_option<'de, T: Deserialize<'de>, D: Deserializer<'de>>(de: D) -> Result<Option<Option<T>>, D::Error> {
    Deserialize::deserialize(de).map(Some)
}

/// Move a keyword across a photo's lists: rename, merge, re-parent, or remove (`to`
/// absent). Paths use `|` (`Places|Portugal|Lisbon`); see `keywords`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KwMove {
    pub from: String,
    #[serde(default)]
    pub to: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct Gps {
    pub lat: f64,
    pub lon: f64,
    #[serde(default)]
    pub alt: Option<f64>,
}

/// A metadata change. Absent fields are left alone; `null` clears a field.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Patch {
    #[serde(default)]
    pub rating: Option<i32>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub label: Option<Option<String>>,
    #[serde(default)]
    pub pick: Option<i32>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub title: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub description: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub creator: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub rights: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords_add: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords_remove: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keywords_set: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hkeywords_set: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keywords_move: Option<KwMove>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub gps: Option<Option<Gps>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub city: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub state: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub country: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub country_code: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub sublocation: Option<Option<String>>,
    /// Absolute EXIF orientation (1-8).
    #[serde(default)]
    pub orientation: Option<u8>,
    /// Relative rotation in degrees (90, -90, 180).
    #[serde(default)]
    pub rotate: Option<i32>,
}

fn clean_opt(v: &Option<String>) -> Option<String> {
    v.as_ref().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn rotate_orientation(o: u8, deg: i32) -> u8 {
    // Clockwise rotation steps for the 4 non-mirrored orientations; mirrored ones map
    // through their non-mirrored partner (good enough for camera files).
    let base = match o {
        3 | 4 => 2,
        6 | 5 => 1,
        8 | 7 => 3,
        _ => 0,
    };
    let steps = ((deg.rem_euclid(360)) / 90) as i32;
    match (base + steps).rem_euclid(4) {
        1 => 6,
        2 => 3,
        3 => 8,
        _ => 1,
    }
}

/// Values of the fields Hyperfocal manages, for journaling and undo.
fn snapshot(r: &PhotoRec) -> Value {
    json!({
        "rating": r.rating, "label": r.label, "pick": r.pick, "title": r.title,
        "description": r.description, "creator": r.creator, "rights": r.rights,
        "keywords": r.keywords, "hkeywords": r.hkeywords,
        "gps": r.lat.zip(r.lon).map(|(lat, lon)| json!({"lat": lat, "lon": lon, "alt": r.alt})),
        "city": r.city, "state": r.state, "country": r.country, "country_code": r.country_code,
        "sublocation": r.sublocation, "orientation": r.orientation,
    })
}

/// A patch that restores the fields that differ between `before` and `after`
/// (only what a batch changed is put back, so later edits by other apps survive).
fn patch_from_snapshots(before: &Value, after: &Value) -> Patch {
    let changed = |k: &str| before.get(k) != after.get(k);
    let s = |k: &str| -> Option<Option<String>> {
        changed(k).then(|| before.get(k).and_then(|x| x.as_str()).map(|s| s.to_string()))
    };
    let list = |k: &str| -> Vec<String> {
        before.get(k).and_then(|x| x.as_array()).map(|a| a.iter().filter_map(|i| i.as_str().map(String::from)).collect()).unwrap_or_default()
    };
    let kw = changed("keywords") || changed("hkeywords");
    Patch {
        rating: changed("rating").then(|| before.get("rating").and_then(|x| x.as_i64()).unwrap_or(0) as i32),
        label: s("label"),
        pick: changed("pick").then(|| before.get("pick").and_then(|x| x.as_i64()).unwrap_or(0) as i32),
        title: s("title"),
        description: s("description"),
        creator: s("creator"),
        rights: s("rights"),
        keywords_set: kw.then(|| list("keywords")),
        hkeywords_set: kw.then(|| list("hkeywords")),
        gps: changed("gps").then(|| before.get("gps").and_then(|g| serde_json::from_value::<Gps>(g.clone()).ok())),
        city: s("city"),
        state: s("state"),
        country: s("country"),
        country_code: s("country_code"),
        sublocation: s("sublocation"),
        orientation: if changed("orientation") { before.get("orientation").and_then(|x| x.as_u64()).map(|x| x as u8) } else { None },
        ..Default::default()
    }
}

fn leaf(k: &str) -> &str {
    k.rsplit('|').next().unwrap_or(k).trim()
}

/// Final keyword lists after applying a patch to the current ones.
fn merge_keywords(cur: &PhotoRec, p: &Patch) -> (Vec<String>, Vec<String>) {
    let mut subj: Vec<String> = cur.keywords.clone();
    let mut hier: Vec<String> = cur.hkeywords.clone();
    if let Some(m) = &p.keywords_move {
        let to = m.to.as_deref().map(crate::keywords::split);
        (subj, hier) = crate::keywords::apply_move(&subj, &hier, &crate::keywords::split(&m.from), to.as_deref());
    }
    if let Some(set) = &p.keywords_set {
        subj = Vec::new();
        hier = Vec::new();
        for k in set {
            let k = k.trim();
            if !k.is_empty() && !subj.iter().any(|s| s.eq_ignore_ascii_case(k)) {
                subj.push(k.to_string());
            }
        }
    }
    if let Some(h) = &p.hkeywords_set {
        hier = h.clone();
    }
    for k in &p.keywords_remove {
        let k = k.trim();
        if k.contains('|') {
            hier.retain(|h| !h.eq_ignore_ascii_case(k));
            let l = leaf(k);
            if !hier.iter().any(|h| leaf(h).eq_ignore_ascii_case(l)) {
                subj.retain(|s| !s.eq_ignore_ascii_case(l));
            }
        } else {
            subj.retain(|s| !s.eq_ignore_ascii_case(k));
            hier.retain(|h| !leaf(h).eq_ignore_ascii_case(k));
        }
    }
    for k in &p.keywords_add {
        add_kw(&mut subj, &mut hier, k);
    }
    (subj, hier)
}

fn add_kw(subj: &mut Vec<String>, hier: &mut Vec<String>, k: &str) {
    let k = k.trim().trim_matches('|');
    if k.is_empty() {
        return;
    }
    let l = leaf(k).to_string();
    if !l.is_empty() && !subj.iter().any(|s| s.eq_ignore_ascii_case(&l)) {
        subj.push(l);
    }
    if k.contains('|') && !hier.iter().any(|h| h.eq_ignore_ascii_case(k)) {
        hier.push(k.to_string());
    }
}

fn kw_changed(a: &[String], b: &[String]) -> bool {
    a.len() != b.len() || a.iter().zip(b.iter()).any(|(x, y)| x != y)
}

fn sidecar_backup_dir(state: &AppState) -> PathBuf {
    let now = time::OffsetDateTime::now_utc();
    state.cfg.config_dir.join("backups").join("sidecars").join(format!("{:04}-{:02}", now.year(), now.month() as u8))
}

/// Read the current effective metadata straight from disk (not from the index), so a
/// write never works on stale data after another app changed the sidecar.
fn fresh(state: &AppState, loc: &Located, sidecar: &Path) -> (PhotoRec, Option<Xmp>) {
    let path = scan::photo_path(state, &loc.folder_path, &loc.rec.name);
    let fm = meta::read_file_meta(&path, Kind::from_i32(loc.rec.kind)).unwrap_or(meta::FileMeta { orientation: 1, ..Default::default() });
    let sc = meta::read_sidecar(sidecar);
    let mut rec = loc.rec.clone();
    meta::merge_into(&mut rec, &fm, sc.as_ref());
    (rec, fm.xmp)
}

fn push_s(args: &mut Vec<String>, tag: &str, v: &Option<String>) {
    match clean_opt(v) {
        Some(s) => args.push(format!("-{tag}={}", s.replace(['\n', '\r'], " "))),
        None => args.push(format!("-{tag}=")),
    }
}

fn push_list(args: &mut Vec<String>, tag: &str, list: &[String]) {
    if list.is_empty() {
        args.push(format!("-{tag}="));
    } else {
        for v in list {
            args.push(format!("-{tag}={}", v.replace(['\n', '\r'], " ")));
        }
    }
}

/// Build ExifTool arguments for a patch. Returns (args, after-record).
fn build_args(cur: &PhotoRec, p: &Patch, seed: Option<&Xmp>) -> (Vec<String>, PhotoRec) {
    let mut a: Vec<String> = Vec::new();
    let mut after = cur.clone();

    // When a sidecar is created, carry over what the file's embedded XMP says, so
    // other apps (which read the sidecar instead of the file) see the same values.
    if let Some(x) = seed {
        if let Some(v) = x.rating.filter(|_| p.rating.is_none()) {
            a.push(format!("-XMP-xmp:Rating={v}"));
        }
        if let Some(v) = x.label.as_ref().filter(|_| p.label.is_none()) {
            a.push(format!("-XMP-xmp:Label={v}"));
        }
        if let Some(v) = x.pick.filter(|_| p.pick.is_none()) {
            a.push(format!("-XMP-xmpDM:Pick={v}"));
        }
        if let Some(v) = x.title.as_ref().filter(|_| p.title.is_none()) {
            a.push(format!("-XMP-dc:Title={v}"));
        }
        if let Some(v) = x.description.as_ref().filter(|_| p.description.is_none()) {
            a.push(format!("-XMP-dc:Description={v}"));
        }
    }

    if let Some(r) = p.rating {
        let r = r.clamp(-1, 5);
        a.push(format!("-XMP-xmp:Rating={r}"));
        after.rating = r;
    }
    if let Some(l) = &p.label {
        push_s(&mut a, "XMP-xmp:Label", l);
        after.label = clean_opt(l);
    }
    if let Some(pk) = p.pick {
        let pk = pk.clamp(-1, 1);
        if pk == 0 {
            a.push("-XMP-xmpDM:Pick=".into());
            a.push("-XMP-xmpDM:Good=".into());
        } else {
            a.push(format!("-XMP-xmpDM:Pick={pk}"));
        }
        after.pick = pk;
    }
    for (field, tag, dst) in [
        (&p.title, "XMP-dc:Title", &mut after.title),
        (&p.description, "XMP-dc:Description", &mut after.description),
        (&p.rights, "XMP-dc:Rights", &mut after.rights),
        (&p.city, "XMP-photoshop:City", &mut after.city),
        (&p.state, "XMP-photoshop:State", &mut after.state),
        (&p.country, "XMP-photoshop:Country", &mut after.country),
        (&p.country_code, "XMP-iptcCore:CountryCode", &mut after.country_code),
        (&p.sublocation, "XMP-iptcCore:Location", &mut after.sublocation),
    ] {
        if let Some(v) = field {
            push_s(&mut a, tag, v);
            *dst = clean_opt(v);
        }
    }
    if let Some(c) = &p.creator {
        // dc:creator is an ordered list.
        let list: Vec<String> = clean_opt(c).map(|s| s.split(';').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()).unwrap_or_default();
        push_list(&mut a, "XMP-dc:Creator", &list);
        after.creator = clean_opt(c);
    }
    let touches_kw = p.keywords_set.is_some()
        || p.hkeywords_set.is_some()
        || !p.keywords_add.is_empty()
        || !p.keywords_remove.is_empty()
        || p.keywords_move.is_some();
    let seeded_kw = seed.map(|x| x.subject.is_some() || x.hsubject.is_some()).unwrap_or(false);
    if touches_kw || seeded_kw {
        let (subj, hier) = if touches_kw { merge_keywords(cur, p) } else { (cur.keywords.clone(), cur.hkeywords.clone()) };
        if seeded_kw || kw_changed(&subj, &cur.keywords) {
            push_list(&mut a, "XMP-dc:Subject", &subj);
        }
        if seeded_kw || kw_changed(&hier, &cur.hkeywords) {
            push_list(&mut a, "XMP-lr:HierarchicalSubject", &hier);
        }
        after.keywords = subj;
        after.hkeywords = hier;
    }
    if let Some(g) = &p.gps {
        match g {
            Some(g) if g.lat.is_finite() && g.lon.is_finite() && g.lat.abs() <= 90.0 && g.lon.abs() <= 180.0 => {
                a.push(format!("-XMP-exif:GPSLatitude={:.7}", g.lat));
                a.push(format!("-XMP-exif:GPSLongitude={:.7}", g.lon));
                match g.alt {
                    Some(alt) if alt.is_finite() => {
                        a.push(format!("-XMP-exif:GPSAltitude={:.1}", alt.abs()));
                        a.push(format!("-XMP-exif:GPSAltitudeRef#={}", if alt < 0.0 { 1 } else { 0 }));
                    }
                    _ => {
                        a.push("-XMP-exif:GPSAltitude=".into());
                        a.push("-XMP-exif:GPSAltitudeRef=".into());
                    }
                }
                after.lat = Some(g.lat);
                after.lon = Some(g.lon);
                after.alt = g.alt;
            }
            _ => {
                for t in ["GPSLatitude", "GPSLongitude", "GPSAltitude", "GPSAltitudeRef"] {
                    a.push(format!("-XMP-exif:{t}="));
                }
                after.lat = None;
                after.lon = None;
                after.alt = None;
            }
        }
    }
    let new_orient = match (p.orientation, p.rotate) {
        (Some(o), _) if (1..=8).contains(&o) => Some(o),
        (_, Some(deg)) if deg % 90 == 0 && deg % 360 != 0 => Some(rotate_orientation(cur.orientation as u8, deg)),
        _ => None,
    };
    if let Some(o) = new_orient {
        a.push(format!("-XMP-tiff:Orientation#={o}"));
        after.orientation = o as i32;
    }
    (a, after)
}

#[derive(Debug, Serialize)]
pub struct BatchResult {
    pub batch: String,
    pub ok: Vec<i64>,
    pub failed: Vec<(i64, String)>,
    /// Set when the change runs in the background (see `JobStatus`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job: Option<JobStatus>,
}

/// A long change (a keyword moved across the library, GPS from a track, a large undo):
/// written in the background one photo at a time, so other edits are never held up
/// for long, journaled as one batch, with progress events for the UI.
#[derive(Debug, Clone, Serialize)]
pub struct JobStatus {
    pub id: String,
    pub label: String,
    pub total: usize,
    pub done: usize,
    pub failed: usize,
    pub finished: bool,
    /// The journal batch: undoing it undoes the whole job.
    pub batch: String,
    /// The first errors, for the UI.
    pub errors: Vec<(i64, String)>,
    pub started: i64,
}

struct Job {
    state: Arc<AppState>,
    status: JobStatus,
    last: std::time::Instant,
}

impl Job {
    fn start(state: &Arc<AppState>, label: String, total: usize, batch: String) -> Job {
        let status = JobStatus {
            id: batch.clone(),
            label,
            total,
            done: 0,
            failed: 0,
            finished: false,
            batch,
            errors: Vec::new(),
            started: crate::db::now_ms(),
        };
        let job = Job { state: state.clone(), status, last: std::time::Instant::now() };
        job.publish();
        job
    }

    fn publish(&self) {
        let mut jobs = self.state.jobs.lock();
        // Keep finished jobs for ten minutes, so a page opened later still sees them.
        let cutoff = crate::db::now_ms() - 600_000;
        jobs.retain(|_, j| !j.finished || j.started > cutoff);
        jobs.insert(self.status.id.clone(), self.status.clone());
        drop(jobs);
        self.state.emit(Event::Job(self.status.clone()));
    }

    fn tick(&mut self, err: Option<(i64, String)>) {
        self.status.done += 1;
        if let Some(e) = err {
            self.status.failed += 1;
            if self.status.errors.len() < 20 {
                self.status.errors.push(e);
            }
        }
        if self.last.elapsed() > std::time::Duration::from_millis(300) {
            self.last = std::time::Instant::now();
            self.publish();
        }
    }

    fn finish(mut self) -> JobStatus {
        self.status.finished = true;
        self.publish();
        self.status
    }
}

/// One photo's change in a batch written by `write_items`.
struct Item {
    id: i64,
    patch: Patch,
    /// The journal row this change undoes, marked undone when it lands.
    undoes: Option<i64>,
}

/// The folder of each photo.
fn folders_of(state: &AppState, ids: impl Iterator<Item = i64>) -> Result<std::collections::HashMap<i64, i64>> {
    let c = state.db.get()?;
    let ids: Vec<i64> = ids.collect();
    let mut out = std::collections::HashMap::with_capacity(ids.len());
    for chunk in ids.chunks(500) {
        let sql = format!("SELECT id, folder_id FROM photos WHERE id IN ({})", chunk.iter().map(|_| "?").collect::<Vec<_>>().join(","));
        let mut st = c.prepare(&sql)?;
        let rows = st.query_map(rusqlite::params_from_iter(chunk.iter()), |r| Ok((r.get(0)?, r.get(1)?)))?;
        for r in rows {
            let (id, f) = r?;
            out.insert(id, f);
        }
    }
    Ok(out)
}

async fn rescan(state: &Arc<AppState>, folder: i64) {
    let st = state.clone();
    if let Ok(Err(e)) = tokio::task::spawn_blocking(move || scan::scan_folder(&st, folder, false)).await {
        tracing::warn!("re-reading folder {folder} after a change: {e:#}");
    }
}

/// Write per-photo changes as one batch. The write lock is taken per photo, so other
/// edits interleave, and each folder is re-read as soon as its photos are done.
async fn write_items(state: &Arc<AppState>, mut items: Vec<Item>, batch: &str, undo_of: Option<&str>, mut job: Option<&mut Job>) -> BatchResult {
    let folder_of = folders_of(state, items.iter().map(|i| i.id)).unwrap_or_default();
    // Stable: a photo listed twice keeps its order.
    items.sort_by_key(|i| folder_of.get(&i.id).copied().unwrap_or(i64::MAX));
    let mut ok = Vec::new();
    let mut failed = Vec::new();
    let mut pending: Option<i64> = None;
    for item in &items {
        let summary = if undo_of.is_some() { format!("undo: {}", summarize(&item.patch)) } else { summarize(&item.patch) };
        let res = if state.is_readonly() {
            Err(anyhow!("Hyperfocal is in read-only mode"))
        } else {
            let _w = state.write_lock.lock().await;
            write_one(state, item.id, &item.patch, batch, undo_of, &summary).await
        };
        let err = match res {
            Ok(fid) => {
                ok.push(item.id);
                if let Some(jid) = item.undoes
                    && let Ok(c) = state.db.get()
                {
                    let _ = c.execute("UPDATE journal SET undone = 1 WHERE id = ?1", [jid]);
                }
                if let Some(prev) = pending.filter(|p| *p != fid) {
                    rescan(state, prev).await;
                }
                pending = Some(fid);
                None
            }
            Err(e) => {
                let msg = format!("{e:#}");
                failed.push((item.id, msg.clone()));
                Some((item.id, msg))
            }
        };
        if let Some(j) = job.as_deref_mut() {
            j.tick(err);
        }
    }
    if let Some(f) = pending {
        rescan(state, f).await;
    }
    for chunk in ok.chunks(5000) {
        state.emit(Event::Photos { ids: chunk.to_vec() });
    }
    BatchResult { batch: batch.to_string(), ok, failed, job: None }
}

/// Start a background job that writes `items` (photo, change) as one batch.
pub fn spawn_patch_job(state: &Arc<AppState>, label: String, items: Vec<(i64, Patch)>) -> Result<JobStatus> {
    if state.is_readonly() {
        bail!("Hyperfocal is in read-only mode");
    }
    let batch = new_batch();
    let mut job = Job::start(state, label, items.len(), batch.clone());
    let status = job.status.clone();
    let st = state.clone();
    let items: Vec<Item> = items.into_iter().map(|(id, patch)| Item { id, patch, undoes: None }).collect();
    tokio::spawn(async move {
        write_items(&st, items, &batch, None, Some(&mut job)).await;
        job.finish();
    });
    Ok(status)
}

pub fn new_batch() -> String {
    format!("{:x}{:04x}", crate::db::now_ms(), rand::random::<u16>())
}

pub fn journal(state: &AppState, batch: &str, kind: &str, photo_id: Option<i64>, summary: &str, data: &Value) -> Result<()> {
    let c = state.db.get()?;
    c.execute(
        "INSERT INTO journal(batch, ts, kind, photo_id, summary, data) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
        params![batch, crate::db::now_ms(), kind, photo_id, summary, data.to_string()],
    )?;
    Ok(())
}

fn summarize(p: &Patch) -> String {
    let mut parts = Vec::new();
    if let Some(r) = p.rating {
        parts.push(if r < 0 { "rejected (rating)".to_string() } else { format!("rating {r}") });
    }
    if let Some(l) = &p.label {
        parts.push(format!("label {}", l.clone().unwrap_or_else(|| "none".into())));
    }
    if let Some(pk) = p.pick {
        parts.push(match pk { 1 => "pick".into(), -1 => "reject flag".into(), _ => "unflag".into() });
    }
    if p.title.is_some() || p.description.is_some() {
        parts.push("title/caption".into());
    }
    if !p.keywords_add.is_empty() {
        parts.push(format!("+{}", p.keywords_add.join(", +")));
    }
    if !p.keywords_remove.is_empty() {
        parts.push(format!("-{}", p.keywords_remove.join(", -")));
    }
    if p.keywords_set.is_some() {
        parts.push("keywords".into());
    }
    if let Some(m) = &p.keywords_move {
        let show = |k: &str| k.replace('|', " › ");
        parts.push(match &m.to {
            Some(t) => format!("keyword {} → {}", show(&m.from), show(t)),
            None => format!("removed keyword {}", show(&m.from)),
        });
    }
    if p.gps.is_some() {
        parts.push("GPS".into());
    }
    if p.city.is_some() || p.state.is_some() || p.country.is_some() || p.sublocation.is_some() {
        parts.push("location".into());
    }
    if p.orientation.is_some() || p.rotate.is_some() {
        parts.push("rotation".into());
    }
    if p.creator.is_some() || p.rights.is_some() {
        parts.push("credit".into());
    }
    parts.join(", ")
}

/// Write a patch to the XMP sidecars of the given photos.
pub async fn apply_meta(state: &Arc<AppState>, ids: &[i64], patch: &Patch, undo_of: Option<&str>) -> Result<BatchResult> {
    apply_meta_in(state, ids, patch, undo_of, None).await
}

async fn apply_meta_in(state: &Arc<AppState>, ids: &[i64], patch: &Patch, undo_of: Option<&str>, batch: Option<String>) -> Result<BatchResult> {
    if state.is_readonly() {
        bail!("Hyperfocal is in read-only mode");
    }
    let _w = state.write_lock.lock().await;
    let batch = batch.unwrap_or_else(new_batch);
    let summary = if undo_of.is_some() { format!("undo: {}", summarize(patch)) } else { summarize(patch) };
    let mut ok = Vec::new();
    let mut failed = Vec::new();
    let mut folders: BTreeSet<i64> = BTreeSet::new();
    for &id in ids {
        match write_one(state, id, patch, &batch, undo_of, &summary).await {
            Ok(fid) => {
                ok.push(id);
                folders.insert(fid);
            }
            Err(e) => failed.push((id, format!("{e:#}"))),
        }
    }
    // Re-read the folders that changed so the index matches the files.
    for fid in folders {
        let st = state.clone();
        let _ = tokio::task::spawn_blocking(move || scan::scan_folder(&st, fid, false)).await;
    }
    if !ok.is_empty() {
        state.emit(Event::Photos { ids: ok.clone() });
    }
    Ok(BatchResult { batch, ok, failed, job: None })
}

async fn write_one(state: &AppState, id: i64, patch: &Patch, batch: &str, undo_of: Option<&str>, summary: &str) -> Result<i64> {
    let loc = {
        let c = state.db.get()?;
        repo::load(&c, id)?.ok_or_else(|| anyhow!("photo {id} not found"))?
    };
    let dir = scan::abs_dir(state, &loc.folder_path);
    // `IMG_1.xmp` (the Adobe/Excire convention) for the photo that owns the name,
    // `IMG_1.TIF.xmp` for another photo with the same name (see `sets`).
    let mut listing = scan::listing(state, &loc.folder_path, loc.folder_is_reject)?;
    // A darktable/digiKam-style sidecar the rescan has not renamed yet gets the
    // standard name first, so the edit lands in it (and Excire sees it).
    let ids = std::collections::HashMap::from([(loc.rec.name.as_str(), loc.rec.id)]);
    for (from, to) in scan::standardize_sidecars(state, &loc.folder_path, &dir, &Plan::new(&listing), &ids, Some(&loc.rec.name)) {
        listing.rename(&from, &to);
    }
    let sidecar_name = Plan::new(&listing).write_sidecar(&loc.rec.name)?;
    let sidecar = dir.join(&sidecar_name);
    let existed = sidecar.exists();
    let (cur, embedded) = fresh(state, &loc, &sidecar);
    let seed = if existed { None } else { embedded.as_ref() };
    let (mut args, after) = build_args(&cur, patch, seed);
    if args.is_empty() {
        return Ok(loc.rec.folder_id);
    }
    // Back up the existing sidecar before touching it.
    let mut backup: Option<String> = None;
    if existed {
        let bdir = sidecar_backup_dir(state);
        std::fs::create_dir_all(&bdir)?;
        let b = bdir.join(format!("{}_{}_{}", id, crate::db::now_ms(), sidecar_name));
        std::fs::copy(&sidecar, &b)?;
        backup = Some(b.to_string_lossy().into_owned());
    }
    args.push("-XMP-xmp:MetadataDate=now".into());
    let mut full = vec!["-overwrite_original".to_string(), "-m".to_string(), "-charset".to_string(), "filename=utf8".to_string()];
    full.extend(args.iter().cloned());
    full.push(sidecar.to_string_lossy().into_owned());
    let out = state.exiftool.lock().await.exec(&full).await?;
    if out.changed() == 0 {
        bail!("{}", out.error().unwrap_or_else(|| format!("exiftool did not write {sidecar_name}")));
    }
    // Verify what landed in the sidecar.
    let check = meta::read_sidecar(&sidecar).unwrap_or_default();
    if patch.rating.is_some() && check.rating.unwrap_or(0) != after.rating {
        tracing::warn!("rating verification mismatch for {}", sidecar.display());
    }
    journal(
        state,
        batch,
        "meta",
        Some(id),
        summary,
        &json!({
            "folder": loc.folder_path, "file": loc.rec.name, "sidecar": sidecar_name,
            "created": !existed, "backup": backup, "undo_of": undo_of,
            "before": snapshot(&cur), "after": snapshot(&after),
        }),
    )?;
    Ok(loc.rec.folder_id)
}

/// Move photos (with their companions) into the reject folder next to them.
pub async fn reject(state: &AppState, ids: &[i64]) -> Result<BatchResult> {
    move_photos(state, ids, true, None, None).await
}

/// Move rejected photos back to the folder their reject folder belongs to.
pub async fn restore(state: &AppState, ids: &[i64]) -> Result<BatchResult> {
    move_photos(state, ids, false, None, None).await
}

async fn move_photos(state: &AppState, ids: &[i64], to_reject: bool, batch: Option<String>, undo_of: Option<&str>) -> Result<BatchResult> {
    if state.is_readonly() {
        bail!("Hyperfocal is in read-only mode");
    }
    let _w = state.write_lock.lock().await;
    let batch = batch.unwrap_or_else(new_batch);
    let mut ok = Vec::new();
    let mut failed = Vec::new();
    let mut touched: HashSet<i64> = HashSet::new();
    for &id in ids {
        let r = move_one(state, id, to_reject, &batch, undo_of);
        match r {
            Ok((from, to)) => {
                ok.push(id);
                touched.insert(from);
                touched.insert(to);
            }
            Err(e) => failed.push((id, format!("{e:#}"))),
        }
    }
    for f in &touched {
        state.emit(Event::Folder { id: *f });
    }
    if !touched.is_empty() {
        state.emit(Event::Tree);
    }
    Ok(BatchResult { batch, ok, failed, job: None })
}

fn move_one(state: &AppState, id: i64, to_reject: bool, batch: &str, undo_of: Option<&str>) -> Result<(i64, i64)> {
    let loc = {
        let c = state.db.get()?;
        repo::load(&c, id)?.ok_or_else(|| anyhow!("photo {id} not found"))?
    };
    let src_dir = scan::abs_dir(state, &loc.folder_path);
    // The photo, the JPEG/HEIF folded into it and the files that belong to them (see
    // `sets`); refuses the JPEG half of a pair.
    let listing = scan::listing(state, &loc.folder_path, loc.folder_is_reject)?;
    let names = Plan::new(&listing).move_set(&loc.rec.name)?;
    let (dst_dir, dst_rel, dst_folder_id) = if to_reject {
        if loc.folder_is_reject {
            bail!("{} is already rejected", loc.rec.name);
        }
        let dst = files::ensure_dir(&src_dir, &state.cfg.reject_dir)?;
        let dname = dst.file_name().and_then(|s| s.to_str()).unwrap_or(&state.cfg.reject_dir).to_string();
        let rel = if loc.folder_path.is_empty() { dname.clone() } else { format!("{}/{}", loc.folder_path, dname) };
        let fid = ensure_folder_row(state, &rel, &dname, Some(loc.rec.folder_id), true)?;
        (dst, rel, fid)
    } else {
        if !loc.folder_is_reject {
            bail!("{} is not in a reject folder", loc.rec.name);
        }
        let parent = loc.folder_parent.ok_or_else(|| anyhow!("reject folder has no parent"))?;
        let rel: String = state.db.get()?.query_row("SELECT path FROM folders WHERE id=?1", [parent], |r| r.get(0))?;
        (scan::abs_dir(state, &rel), rel, parent)
    };
    files::move_group(&src_dir, &dst_dir, &names)?;

    // Keep photo ids (and their caches) by moving the index rows too.
    let mut c = state.db.get()?;
    let tx = c.transaction()?;
    for n in &names {
        tx.execute("DELETE FROM photos WHERE folder_id = ?1 AND name = ?2", params![dst_folder_id, n])?;
        tx.execute("UPDATE photos SET folder_id = ?1 WHERE folder_id = ?2 AND name = ?3", params![dst_folder_id, loc.rec.folder_id, n])?;
        // The integrity monitor's record moves with the file (not missing here, not new there).
        tx.execute("DELETE FROM originals WHERE folder = ?1 AND name = ?2", params![dst_rel, n])?;
        tx.execute("UPDATE originals SET folder = ?1 WHERE folder = ?2 AND name = ?3", params![dst_rel, loc.folder_path, n])?;
    }
    let moved: Vec<(i64, PhotoRec)> = {
        let mut st = tx.prepare(&format!("SELECT {} FROM photos p WHERE p.folder_id = ?1", repo::PHOTO_COLS))?;
        let rows = st.query_map([dst_folder_id], |r| repo::photo_from_row(r))?;
        rows.filter_map(|r| r.ok()).filter(|r| names.contains(&r.name)).map(|r| (r.id, r)).collect()
    };
    for (pid, rec) in &moved {
        repo::index_text(&tx, *pid, rec, &dst_rel)?;
    }
    repo::refresh_folder(&tx, loc.rec.folder_id)?;
    repo::refresh_folder(&tx, dst_folder_id)?;
    tx.commit()?;
    drop(c);
    journal(
        state,
        batch,
        if to_reject { "reject" } else { "restore" },
        Some(id),
        if to_reject { "moved to reject folder" } else { "restored from reject folder" },
        &json!({ "from": loc.folder_path, "to": dst_rel, "files": names, "undo_of": undo_of }),
    )?;
    Ok((loc.rec.folder_id, dst_folder_id))
}

fn ensure_folder_row(state: &AppState, rel: &str, name: &str, parent: Option<i64>, is_reject: bool) -> Result<i64> {
    let c = state.db.get()?;
    if let Ok(id) = c.query_row("SELECT id FROM folders WHERE path = ?1", [rel], |r| r.get::<_, i64>(0)) {
        return Ok(id);
    }
    Ok(c.query_row(
        "INSERT INTO folders(parent_id, path, name, is_reject, seen, scanned_at) VALUES(?1, ?2, ?3, ?4, 1, ?5) RETURNING id",
        params![parent, rel, name, is_reject as i64, crate::db::now_ms()],
        |r| r.get(0),
    )?)
}

/// Undo a batch: metadata batches write their previous values back, moves are moved
/// back. Returns the new batch of the undo itself.
pub async fn undo(state: &Arc<AppState>, batch: &str) -> Result<BatchResult> {
    undo_in(state, batch, None).await
}

/// Journal rows of a batch that can still be undone.
pub fn undo_count(state: &AppState, batch: &str) -> Result<usize> {
    let c = state.db.get()?;
    let n: i64 = c.query_row("SELECT count(*) FROM journal WHERE batch = ?1 AND undone = 0", [batch], |r| r.get(0))?;
    Ok(n as usize)
}

/// Undo a large batch in the background.
pub fn spawn_undo(state: &Arc<AppState>, batch: &str, label: String) -> Result<JobStatus> {
    let total = undo_count(state, batch)?;
    if total == 0 {
        bail!("nothing to undo");
    }
    let new = new_batch();
    let job = Job::start(state, label, total, new);
    let status = job.status.clone();
    let (st, batch) = (state.clone(), batch.to_string());
    tokio::spawn(async move {
        if let Err(e) = undo_in(&st, &batch, Some(job)).await {
            tracing::warn!("undo of {batch} failed: {e:#}");
        }
    });
    Ok(status)
}

async fn undo_in(state: &Arc<AppState>, batch: &str, mut job: Option<Job>) -> Result<BatchResult> {
    let rows: Vec<(i64, String, Option<i64>, String)> = {
        let c = state.db.get()?;
        let mut st = c.prepare("SELECT id, kind, photo_id, data FROM journal WHERE batch = ?1 AND undone = 0 ORDER BY id DESC")?;
        let r = st.query_map([batch], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
        r.collect::<std::result::Result<_, _>>()?
    };
    if rows.is_empty() {
        if let Some(j) = job {
            j.finish();
        }
        bail!("nothing to undo");
    }
    let mut ok = Vec::new();
    let mut failed = Vec::new();
    let new_batch = job.as_ref().map(|j| j.status.batch.clone()).unwrap_or_else(new_batch);
    // Metadata rows are written together (each folder re-read once); renames and moves
    // are undone as they come.
    let mut meta: Vec<Item> = Vec::new();
    for (jid, kind, pid, data) in rows {
        let v: Value = serde_json::from_str(&data).unwrap_or(Value::Null);
        let mut err: Option<(i64, String)> = None;
        if kind == "rename" {
            match undo_rename(state, jid, &v).await {
                Ok(id) => ok.extend(id.or(pid)),
                Err(e) => err = Some((pid.unwrap_or(0), format!("{e:#}"))),
            }
        } else if let Some(pid) = pid {
            match kind.as_str() {
                "reject" | "restore" => {
                    match move_photos(state, &[pid], kind == "restore", Some(new_batch.clone()), Some(batch)).await {
                        Ok(r) if r.failed.is_empty() => {
                            ok.push(pid);
                            state.db.get()?.execute("UPDATE journal SET undone = 1 WHERE id = ?1", [jid])?;
                        }
                        Ok(r) => err = r.failed.into_iter().next(),
                        Err(e) => err = Some((pid, format!("{e:#}"))),
                    }
                }
                _ => {
                    let before = v.get("before").cloned().unwrap_or(Value::Null);
                    let after = v.get("after").cloned().unwrap_or(Value::Null);
                    meta.push(Item { id: pid, patch: patch_from_snapshots(&before, &after), undoes: Some(jid) });
                    continue;
                }
            }
        } else {
            continue;
        }
        if let Some(e) = &err {
            failed.push(e.clone());
        }
        if let Some(j) = job.as_mut() {
            j.tick(err);
        }
    }
    if !meta.is_empty() {
        let r = write_items(state, meta, &new_batch, Some(batch), job.as_mut()).await;
        ok.extend(r.ok);
        failed.extend(r.failed);
    }
    if let Some(j) = job {
        j.finish();
    }
    Ok(BatchResult { batch: new_batch, ok, failed, job: None })
}

/// Give a renamed sidecar its old name back. Marked undone first, so the rescan that
/// follows leaves it alone from now on.
async fn undo_rename(state: &Arc<AppState>, jid: i64, v: &Value) -> Result<Option<i64>> {
    let field = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string).ok_or_else(|| anyhow!("bad journal entry"));
    let (folder, from, to) = (field("folder")?, field("from")?, field("to")?);
    let dir = scan::abs_dir(state, &folder);
    files::rename_noreplace(&dir.join(&to), &dir.join(&from)).with_context(|| format!("renaming {to} back to {from}"))?;
    state.db.get()?.execute("UPDATE journal SET undone = 1 WHERE id = ?1", [jid])?;
    let fid: Option<i64> = state.db.get()?.query_row("SELECT id FROM folders WHERE path = ?1", [&folder], |r| r.get(0)).ok();
    let Some(fid) = fid else { return Ok(None) };
    let st = state.clone();
    tokio::task::spawn_blocking(move || scan::scan_folder(&st, fid, false)).await??;
    let photo = v.get("photo").and_then(Value::as_str).unwrap_or_default();
    let c = state.db.get()?;
    Ok(c.query_row("SELECT id FROM photos WHERE folder_id = ?1 AND name = ?2", params![fid, photo], |r| r.get(0)).ok())
}

pub fn recent_journal(state: &AppState, limit: usize) -> Result<Vec<Value>> {
    let c = state.db.get()?;
    let mut st = c.prepare(
        "SELECT batch, min(ts), kind, count(*), max(summary), min(undone), max(json_extract(data, '$.undo_of'))
         FROM journal GROUP BY batch ORDER BY min(id) DESC LIMIT ?1",
    )?;
    let rows = st.query_map([limit as i64], |r| {
        Ok(json!({
            "batch": r.get::<_, String>(0)?, "ts": r.get::<_, i64>(1)?, "kind": r.get::<_, String>(2)?,
            "count": r.get::<_, i64>(3)?, "summary": r.get::<_, Option<String>>(4)?, "undone": r.get::<_, i64>(5)? != 0,
            "undo_of": r.get::<_, Option<String>>(6)?,
        }))
    })?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation() {
        assert_eq!(rotate_orientation(1, 90), 6);
        assert_eq!(rotate_orientation(6, 90), 3);
        assert_eq!(rotate_orientation(3, 90), 8);
        assert_eq!(rotate_orientation(8, 90), 1);
        assert_eq!(rotate_orientation(1, -90), 8);
        assert_eq!(rotate_orientation(6, 180), 8);
    }

    #[test]
    fn keywords() {
        let cur = PhotoRec { keywords: vec!["Dog".into(), "Beach".into()], hkeywords: vec!["Animals|Dog".into()], ..Default::default() };
        let p = Patch { keywords_add: vec!["Places|Portugal|Lisbon".into()], keywords_remove: vec!["Dog".into()], ..Default::default() };
        let (s, h) = merge_keywords(&cur, &p);
        assert_eq!(s, vec!["Beach", "Lisbon"]);
        assert_eq!(h, vec!["Places|Portugal|Lisbon"]);
    }

    #[test]
    fn patch_json() {
        let p: Patch = serde_json::from_str(r#"{"rating":3,"label":null,"gps":{"lat":1.5,"lon":2.5}}"#).unwrap();
        assert_eq!(p.rating, Some(3));
        assert_eq!(p.label, Some(None));
        assert!(p.title.is_none());
        assert_eq!(p.gps, Some(Some(Gps { lat: 1.5, lon: 2.5, alt: None })));
    }

    #[test]
    fn args_for_new_sidecar_seed() {
        let cur = PhotoRec { rating: 3, ..Default::default() };
        let seed = Xmp { rating: Some(3), ..Default::default() };
        let (a, after) = build_args(&cur, &Patch { label: Some(Some("Red".into())), ..Default::default() }, Some(&seed));
        assert!(a.contains(&"-XMP-xmp:Rating=3".to_string()));
        assert!(a.contains(&"-XMP-xmp:Label=Red".to_string()));
        assert_eq!(after.label.as_deref(), Some("Red"));
    }
}

#[cfg(test)]
mod undo_tests {
    use super::*;
    #[test]
    fn undo_patch_only_changed_fields() {
        let before = json!({"rating": 0, "label": null, "keywords": ["A"], "hkeywords": [], "gps": null, "city": "X"});
        let after = json!({"rating": 3, "label": "Red", "keywords": ["A"], "hkeywords": [], "gps": null, "city": "X"});
        let p = patch_from_snapshots(&before, &after);
        assert_eq!(p.rating, Some(0));
        assert_eq!(p.label, Some(None));
        assert!(p.keywords_set.is_none());
        assert!(p.gps.is_none());
        assert!(p.city.is_none());
    }
}
