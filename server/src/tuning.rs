//! Advanced settings: the numbers behind series matching, the best frame, culling's
//! possible rejects, Find similar and the Duplicates report, tunable per library
//! (Settings → Advanced). Stored in the index (`meta` 'tuning', JSON); anything missing
//! or out of range falls back to its default, so an old or hand-edited value never
//! breaks the app.
//!
//! The defaults for series were calibrated on a hand-labelled real sample (an event, a
//! wildlife walk, pets, flowers, the moon: 669 Canon R6 RAWs): 98% of the frames that
//! belong together are joined, and no two scenes are.

use crate::state::{AppState, Event};
use anyhow::Result;
use parking_lot::RwLock;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::{Arc, LazyLock};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tuning {
    // ------------------------------------------------ series
    /// Frames at most this many seconds apart stay in one series unless the scene clearly
    /// changes.
    pub series_burst: f64,
    /// The longest pause within a series, seconds. After a pause the frames must look
    /// alike, more so the longer it was.
    pub series_gap: f64,
    /// How alike frames after a pause must look: 0 loose .. 1 strict.
    pub series_strictness: f64,
    /// Within a burst, frames less alike than this (0..1) start a new series (0: a burst
    /// is never split).
    pub series_scene: f64,
    /// Fewest frames that make a series.
    pub series_min: i64,
    /// Series may mix cameras (two bodies on one subject).
    pub series_cameras: bool,
    // ------------------------------------------------ best frame
    /// Open eyes count before sharpness (else sharpness first).
    pub best_eyes_first: bool,
    /// Sharpness differences smaller than this (0..1) tell nothing apart.
    pub best_tie: f64,
    // ------------------------------------------------ possible rejects
    /// Subjects less sharp than this (0..1) are blurry.
    pub cull_blurry: f64,
    /// Subjects at least this sharp are called sharp.
    pub cull_sharp: f64,
    /// Eyes count as closed from this score (0 open .. 1 closed).
    pub cull_closed: f64,
    /// Highlights are clipped when this share of the photo is.
    pub cull_clipped: f64,
    /// Too dark when this share of the photo is underexposed.
    pub cull_dark: f64,
    // ------------------------------------------------ similar, search, duplicates
    /// Find similar keeps photos at least this alike (cosine).
    pub like_floor: f64,
    /// Search by content keeps photos at least this close to the description (cosine).
    pub search_floor: f64,
    /// Near-duplicates: perceptual-hash bits apart at strict / normal / loose.
    pub near_strict: i64,
    pub near_normal: i64,
    pub near_loose: i64,
    /// Look-alikes: cosine at strict / normal / loose.
    pub alike_strict: f64,
    pub alike_normal: f64,
    pub alike_loose: f64,
}

impl Default for Tuning {
    fn default() -> Tuning {
        Tuning {
            series_burst: 2.0,
            series_gap: 60.0,
            series_strictness: 0.5,
            series_scene: 0.1,
            series_min: 2,
            series_cameras: false,
            best_eyes_first: true,
            best_tie: 0.05,
            cull_blurry: 0.3,
            cull_sharp: 0.7,
            cull_closed: 0.45,
            cull_clipped: 0.05,
            cull_dark: 0.5,
            like_floor: 0.6,
            search_floor: 0.075,
            near_strict: 4,
            near_normal: 7,
            near_loose: 10,
            alike_strict: 0.95,
            alike_normal: 0.92,
            alike_loose: 0.88,
        }
    }
}

fn clampf(v: f64, lo: f64, hi: f64, def: f64) -> f64 {
    if v.is_finite() { v.clamp(lo, hi) } else { def }
}

impl Tuning {
    /// Every value within its range, and the pairs that must be ordered, ordered.
    pub fn clamped(mut self) -> Tuning {
        let d = Tuning::default();
        self.series_burst = clampf(self.series_burst, 0.0, 10.0, d.series_burst);
        self.series_gap = clampf(self.series_gap, 0.0, 900.0, d.series_gap).max(self.series_burst);
        self.series_strictness = clampf(self.series_strictness, 0.0, 1.0, d.series_strictness);
        self.series_scene = clampf(self.series_scene, 0.0, 1.0, d.series_scene);
        self.series_min = self.series_min.clamp(2, 50);
        self.best_tie = clampf(self.best_tie, 0.0, 0.3, d.best_tie);
        self.cull_blurry = clampf(self.cull_blurry, 0.0, 1.0, d.cull_blurry);
        self.cull_sharp = clampf(self.cull_sharp, 0.0, 1.0, d.cull_sharp).max(self.cull_blurry);
        self.cull_closed = clampf(self.cull_closed, 0.05, 0.95, d.cull_closed);
        self.cull_clipped = clampf(self.cull_clipped, 0.001, 0.5, d.cull_clipped);
        self.cull_dark = clampf(self.cull_dark, 0.05, 1.0, d.cull_dark);
        self.like_floor = clampf(self.like_floor, 0.3, 0.98, d.like_floor);
        self.search_floor = clampf(self.search_floor, 0.0, 0.2, d.search_floor);
        self.near_strict = self.near_strict.clamp(0, 20);
        self.near_normal = self.near_normal.clamp(self.near_strict, 24);
        self.near_loose = self.near_loose.clamp(self.near_normal, 28);
        self.alike_strict = clampf(self.alike_strict, 0.7, 0.999, d.alike_strict);
        self.alike_normal = clampf(self.alike_normal, 0.7, self.alike_strict, d.alike_normal);
        self.alike_loose = clampf(self.alike_loose, 0.7, self.alike_normal, d.alike_loose);
        self
    }

    /// How series are cut.
    pub fn series_rules(&self) -> crate::dupes::bursts::Rules {
        crate::dupes::bursts::Rules {
            burst_gap: self.series_burst,
            max_gap: self.series_gap,
            strictness: self.series_strictness as f32,
            scene: self.series_scene as f32,
            min_len: self.series_min as usize,
            cameras: self.series_cameras,
        }
    }

    /// The culling thresholds that decide the possible rejects.
    pub fn cull_rules(&self) -> crate::cull::Thresholds {
        crate::cull::Thresholds { blurry: self.cull_blurry, closed: self.cull_closed, clipped: self.cull_clipped, dark: self.cull_dark }
    }
}

static CURRENT: LazyLock<RwLock<Arc<Tuning>>> = LazyLock::new(|| RwLock::new(Arc::new(Tuning::default())));

/// The tuning in effect.
pub fn get() -> Arc<Tuning> {
    CURRENT.read().clone()
}

fn set(t: Tuning) {
    *CURRENT.write() = Arc::new(t);
}

/// The stored tuning (defaults for what is missing).
pub fn load(c: &Connection) -> Tuning {
    crate::db::get_meta(c, "tuning")
        .and_then(|s| serde_json::from_str::<Tuning>(&s).ok())
        .unwrap_or_default()
        .clamped()
}

/// Reads the stored tuning at startup.
pub fn init(c: &Connection) {
    set(load(c));
}

/// Stores a new tuning and puts it in effect: possible rejects are worked out again from
/// the stored measurements when their thresholds changed (nothing is measured again).
pub fn apply(state: &AppState, c: &Connection, next: Tuning) -> Result<Tuning> {
    let next = next.clamped();
    let prev = get();
    crate::db::set_meta(c, "tuning", &serde_json::to_string(&next)?)?;
    set(next.clone());
    let (a, b) = (prev.cull_rules(), next.cull_rules());
    if a != b {
        crate::cull::rederive(c, &b, a.closed != b.closed)?;
    }
    state.emit(Event::Tuning);
    Ok(next)
}

/// Merges a partial JSON object into the tuning (unknown keys and wrong types are
/// refused).
pub fn merged(base: &Tuning, patch: &Value) -> std::result::Result<Tuning, String> {
    let Value::Object(p) = patch else { return Err("the tuning must be an object".into()) };
    let mut v = serde_json::to_value(base).map_err(|e| e.to_string())?;
    let obj = v.as_object_mut().expect("tuning is an object");
    for (k, val) in p {
        let Some(slot) = obj.get_mut(k) else { return Err(format!("unknown setting {k:?}")) };
        let ok = match (&*slot, val) {
            (Value::Bool(_), Value::Bool(_)) => true,
            (Value::Number(_), Value::Number(n)) => n.as_f64().is_some_and(f64::is_finite),
            _ => false,
        };
        if !ok {
            return Err(format!("{k} has the wrong type"));
        }
        *slot = val.clone();
    }
    serde_json::from_value(v).map_err(|e| e.to_string())
}

pub fn json(t: &Tuning) -> Value {
    json!({ "values": t, "defaults": Tuning::default() })
}

// ------------------------------------------------------------------ API

use crate::api::{ApiResult, St, bad};
use axum::extract::{Query, State};
use axum::routing;
use axum::{Json, Router};

pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/tuning", routing::get(get_tuning).put(put_tuning).delete(reset_tuning)).route("/tuning/series", routing::get(preview_series))
}

async fn get_tuning() -> Json<Value> {
    Json(json(&get()))
}

async fn store(state: Arc<AppState>, next: Tuning) -> ApiResult<Json<Value>> {
    let v = tokio::task::spawn_blocking(move || -> Result<Value> {
        let c = state.db.get()?;
        Ok(json(&apply(&state, &c, next)?))
    })
    .await
    .map_err(|e| anyhow::anyhow!("{e}"))??;
    Ok(Json(v))
}

/// Changes some settings: `{"series_gap": 30, ...}`.
async fn put_tuning(State(state): St, Json(patch): Json<Value>) -> ApiResult<Json<Value>> {
    let next = merged(&get(), &patch).map_err(bad)?;
    store(state, next).await
}

/// Every setting back to its default.
async fn reset_tuning(State(state): St) -> ApiResult<Json<Value>> {
    store(state, Tuning::default()).await
}

#[derive(serde::Deserialize)]
pub struct PreviewQuery {
    folder: Option<i64>,
    burst: Option<f64>,
    gap: Option<f64>,
    strictness: Option<f64>,
    scene: Option<f64>,
    min: Option<i64>,
    cameras: Option<u8>,
}

#[derive(Serialize, Default, Debug, PartialEq)]
pub struct SeriesStats {
    /// Photos with a capture time.
    pub photos: usize,
    pub series: usize,
    /// Photos in a series.
    pub in_series: usize,
    pub largest: usize,
}

/// What series the settings would make of a folder (and the folders below it), next to
/// what the settings in effect make: Settings → Advanced shows it while a value moves.
async fn preview_series(State(state): St, Query(q): Query<PreviewQuery>) -> ApiResult<Json<Value>> {
    let now = get();
    let candidate = Tuning {
        series_burst: q.burst.unwrap_or(now.series_burst),
        series_gap: q.gap.unwrap_or(now.series_gap),
        series_strictness: q.strictness.unwrap_or(now.series_strictness),
        series_scene: q.scene.unwrap_or(now.series_scene),
        series_min: q.min.unwrap_or(now.series_min),
        series_cameras: q.cameras.map(|c| c == 1).unwrap_or(now.series_cameras),
        ..(*now).clone()
    }
    .clamped();
    let v = tokio::task::spawn_blocking(move || -> Result<Value> {
        let t = std::time::Instant::now();
        let c = state.db.get()?;
        let frames = series_frames(&c, q.folder)?;
        let ix = state.visual.index.read();
        let stats = |rules| series_stats(&frames, rules, &ix);
        Ok(json!({
            "now": stats(now.series_rules()),
            "candidate": stats(candidate.series_rules()),
            "ms": t.elapsed().as_millis() as u64,
        }))
    })
    .await
    .map_err(|e| anyhow::anyhow!("{e}"))??;
    Ok(Json(v))
}

/// The frames series are made of: shown photos with a capture time, outside the reject
/// folders, in a folder and below it (or the whole library).
fn series_frames(c: &Connection, folder: Option<i64>) -> Result<Vec<crate::cull::series::Frame>> {
    let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    let scope = match folder {
        Some(f) => crate::dupes::report::subtree(&crate::dupes::report::folder_path(c, f)?, &mut args),
        None => "1".into(),
    };
    let sql = format!(
        "SELECT p.id, p.folder_id, COALESCE(p.make, '') || ' ' || COALESCE(p.model, ''), p.taken, p.name, h.hash
         FROM photos p JOIN folders f ON f.id = p.folder_id LEFT JOIN phashes h ON h.photo_id = p.id
         WHERE p.pair_of IS NULL AND f.is_reject = 0 AND p.taken IS NOT NULL AND {scope}"
    );
    let mut st = c.prepare(&sql)?;
    let rows = st.query_map(rusqlite::params_from_iter(args.iter().map(|b| b.as_ref())), |r| {
        Ok(crate::cull::series::Frame {
            id: r.get(0)?,
            folder: r.get(1)?,
            camera: r.get(2)?,
            taken: r.get(3)?,
            name: r.get(4)?,
            hash: r.get::<_, Option<i64>>(5)?.map(|h| h as u64),
            ..Default::default()
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn series_stats(frames: &[crate::cull::series::Frame], rules: crate::dupes::bursts::Rules, ix: &crate::visual::Index) -> SeriesStats {
    let mut frames = frames.to_vec();
    for f in &mut frames {
        f.emb = ix.slot(f.id);
    }
    let looks: Option<&dyn crate::dupes::bursts::Looks> = if ix.is_empty() { None } else { Some(ix) };
    let groups = crate::cull::series::group(&frames, rules, looks);
    SeriesStats {
        photos: frames.len(),
        series: groups.len(),
        in_series: groups.iter().map(Vec::len).sum(),
        largest: groups.iter().map(Vec::len).max().unwrap_or(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_stay_in_range_and_in_order() {
        let t = Tuning { series_gap: -5.0, series_burst: 8.0, cull_sharp: 0.1, cull_blurry: 0.4, near_normal: 2, near_strict: 6, alike_loose: 0.99, series_min: 0, like_floor: f64::NAN, ..Default::default() }.clamped();
        assert_eq!(t.series_burst, 8.0);
        assert_eq!(t.series_gap, 8.0);
        assert_eq!(t.cull_sharp, 0.4);
        assert_eq!(t.near_normal, 6);
        assert!(t.alike_loose <= t.alike_normal);
        assert_eq!(t.series_min, 2);
        assert_eq!(t.like_floor, 0.6);
        assert_eq!(Tuning::default().clamped(), Tuning::default());
    }

    #[test]
    fn patches_merge_and_bad_ones_are_refused() {
        let d = Tuning::default();
        let t = merged(&d, &json!({ "series_gap": 30, "series_cameras": true })).unwrap();
        assert_eq!(t.series_gap, 30.0);
        assert!(t.series_cameras);
        assert_eq!(t.cull_blurry, d.cull_blurry);
        assert!(merged(&d, &json!({ "nope": 1 })).is_err());
        assert!(merged(&d, &json!({ "series_gap": "30" })).is_err());
        assert!(merged(&d, &json!({ "series_cameras": 1 })).is_err());
        assert!(merged(&d, &json!([1, 2])).is_err());
        // A stored value from an older version keeps what it knows.
        let old: Tuning = serde_json::from_str(r#"{"series_gap": 12, "gone_setting": 3}"#).unwrap();
        assert_eq!(old.series_gap, 12.0);
        assert_eq!(old.series_burst, d.series_burst);
    }
}
