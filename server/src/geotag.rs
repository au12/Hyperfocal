//! Geotagging from a track: upload track files, preview where each photo falls on
//! them, then write the positions to the photos' sidecars as one background change
//! that undoes as a whole.
//!
//! A photo's capture time is the camera's clock. Its moment in UTC is that time minus
//! the camera's time zone: the zone written into the photo when the camera records it
//! (OffsetTimeOriginal), else the zone chosen for the camera, or the one under which
//! the most photos fall on the track.

use crate::api::{ApiError, ApiResult, St, bad, check_writable};
use crate::ops::{self, Gps, Patch};
use crate::state::AppState;
use crate::tracks::{self, Fix};
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{delete, post};
use axum::{Json, Router};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;

/// An uploaded track, kept in memory for a while.
pub struct StoredTrack {
    pub name: String,
    pub format: &'static str,
    pub fixes: Arc<Vec<Fix>>,
    pub used: Instant,
}

const MAX_UPLOAD: u64 = 2 << 30;
const KEEP: Duration = Duration::from_secs(3 * 3600);
const MAX_TRACKS: usize = 24;
const MAX_FIXES: usize = 40_000_000;
const MAX_PHOTOS: usize = 500_000;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/geotag/tracks", post(upload))
        .route("/geotag/tracks/{id}", delete(forget))
        .route("/geotag/preview", post(preview))
        .route("/geotag/apply", post(apply))
}

/// Removes a file when dropped (the spooled upload, however the request ends).
struct TempFile(std::path::PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[derive(Deserialize)]
struct UploadQuery {
    name: Option<String>,
}

/// Drop tracks unused for a few hours, and the oldest past the limits.
fn prune(map: &mut std::collections::HashMap<String, StoredTrack>) {
    map.retain(|_, t| t.used.elapsed() < KEEP);
    while map.len() > MAX_TRACKS || map.values().map(|t| t.fixes.len()).sum::<usize>() > MAX_FIXES {
        let Some(oldest) = map.iter().min_by_key(|(_, t)| t.used).map(|(k, _)| k.clone()) else { break };
        map.remove(&oldest);
    }
}

async fn upload(State(state): St, Query(q): Query<UploadQuery>, body: Body) -> ApiResult<Json<Value>> {
    // Spooled to a file: a location history can be larger than memory should hold.
    let dir = state.cfg.cache_dir.join("tmp");
    tokio::fs::create_dir_all(&dir).await.map_err(anyhow::Error::from)?;
    let path = dir.join(format!("track-{}", ops::new_batch()));
    let tmp = TempFile(path.clone());
    let file = tokio::fs::File::create(&path).await.map_err(anyhow::Error::from)?;
    let mut w = tokio::io::BufWriter::with_capacity(1 << 20, file);
    let mut stream = body.into_data_stream();
    let mut total = 0u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| bad(format!("the upload was interrupted: {e}")))?;
        total += chunk.len() as u64;
        if total > MAX_UPLOAD {
            return Err(ApiError(StatusCode::PAYLOAD_TOO_LARGE, "track files are limited to 2 GB".into()));
        }
        w.write_all(&chunk).await.map_err(anyhow::Error::from)?;
    }
    w.flush().await.map_err(anyhow::Error::from)?;
    drop(w);
    let track = tokio::task::spawn_blocking(move || -> anyhow::Result<tracks::Track> {
        let f = std::fs::File::open(&path)?;
        tracks::parse(std::io::BufReader::with_capacity(1 << 20, f))
    })
    .await
    .map_err(|e| anyhow::anyhow!("{e}"))?
    .map_err(|e| bad(format!("{e:#}")))?;
    drop(tmp);
    let name: String = q.name.as_deref().unwrap_or("track").trim().chars().filter(|c| !c.is_control()).take(200).collect();
    let id = ops::new_batch();
    let (start, end, n) = (track.fixes[0].t, track.fixes[track.fixes.len() - 1].t, track.fixes.len());
    let out = json!({ "id": id, "name": name, "format": track.format, "points": n, "start": start, "end": end });
    let mut map = state.tracks.lock();
    map.insert(id, StoredTrack { name, format: track.format, fixes: Arc::new(track.fixes), used: Instant::now() });
    prune(&mut map);
    Ok(Json(out))
}

async fn forget(State(state): St, Path(id): Path<String>) -> Json<Value> {
    state.tracks.lock().remove(&id);
    Json(json!({ "ok": true }))
}

fn default_gap() -> i64 {
    30 * 60
}

#[derive(Deserialize)]
struct GeoBody {
    tracks: Vec<String>,
    ids: Vec<i64>,
    /// The camera's time zone in minutes east of UTC, for photos that do not carry
    /// their own; absent: the zone under which the most photos fall on the track.
    #[serde(default)]
    tz: Option<i32>,
    /// Seconds added to the camera's clock (it ran slow: positive).
    #[serde(default)]
    drift: i64,
    /// The longest gap between two fixes (seconds) that a photo may fall into.
    #[serde(default = "default_gap")]
    gap: i64,
    /// Also photos that already have a position.
    #[serde(default)]
    replace: bool,
    /// The browser's time zone (minutes), which wins when several zones fit as well.
    #[serde(default)]
    prefer: i32,
}

/// One photo's capture time as the index has it.
#[derive(Debug, Clone)]
pub struct PhotoTime {
    pub id: i64,
    /// The camera's clock, read as UTC (seconds).
    pub taken: Option<i64>,
    /// The time zone written with the capture time (minutes), if any.
    pub zone: Option<i32>,
    pub has_gps: bool,
}

/// Where each photo falls on the track.
#[derive(Debug, Default)]
pub struct Plan {
    pub tz: i32,
    /// The best zone was searched for: (lowest, highest) of the zones that fit as well.
    pub plateau: Option<(i32, i32)>,
    pub counts: Vec<(i32, usize)>,
    pub matched: Vec<(i64, Fix)>,
    pub own_zone: usize,
    pub no_time: usize,
    pub outside: usize,
    pub has_gps: usize,
    /// First and last moment (UTC ms) of the photos considered.
    pub span: Option<(i64, i64)>,
}

pub fn plan(fixes: &[Fix], photos: &[PhotoTime], tz: Option<i32>, drift: i64, gap_ms: i64, replace: bool, prefer: i32) -> Plan {
    let mut p = Plan::default();
    match tz {
        Some(z) => p.tz = z,
        None => {
            let local: Vec<i64> = photos.iter().filter(|x| x.zone.is_none()).filter_map(|x| x.taken.map(|t| t + drift)).collect();
            let (best, plateau, counts) = tracks::detect_zone(fixes, &local, gap_ms, prefer);
            p.tz = best;
            p.plateau = Some(plateau);
            p.counts = counts;
        }
    }
    for x in photos {
        p.own_zone += x.zone.is_some() as usize;
        if x.has_gps && !replace {
            p.has_gps += 1;
            continue;
        }
        let Some(t) = x.taken else {
            p.no_time += 1;
            continue;
        };
        let utc = (t + drift - x.zone.unwrap_or(p.tz) as i64 * 60) * 1000;
        p.span = Some(p.span.map_or((utc, utc), |(a, b)| (a.min(utc), b.max(utc))));
        match tracks::locate(fixes, utc, gap_ms) {
            Some(f) => p.matched.push((x.id, f)),
            None => p.outside += 1,
        }
    }
    p
}

/// The fixes of the chosen tracks, merged (and marked as used).
fn fixes_of(state: &AppState, ids: &[String]) -> ApiResult<(Vec<Fix>, Vec<String>)> {
    let mut map = state.tracks.lock();
    prune(&mut map);
    let mut parts: Vec<Arc<Vec<Fix>>> = Vec::new();
    let mut names = Vec::new();
    for id in ids {
        let t = map.get_mut(id).ok_or_else(|| ApiError(StatusCode::GONE, "a track file is no longer loaded: add it again".into()))?;
        t.used = Instant::now();
        parts.push(t.fixes.clone());
        names.push(t.name.clone());
    }
    drop(map);
    let fixes = if parts.len() == 1 { parts[0].as_ref().clone() } else { tracks::merge(&parts.iter().map(|p| p.as_slice()).collect::<Vec<_>>()) };
    Ok((fixes, names))
}

fn photo_times(state: &AppState, ids: &[i64]) -> anyhow::Result<Vec<PhotoTime>> {
    let c = state.db.get()?;
    let mut st = c.prepare("SELECT id, taken, taken_str, lat IS NOT NULL AND lon IS NOT NULL FROM photos WHERE id IN (SELECT value FROM json_each(?1))")?;
    let list = serde_json::to_string(ids)?;
    let rows = st.query_map([list], |r| {
        let taken_str: Option<String> = r.get(2)?;
        Ok(PhotoTime { id: r.get(0)?, taken: r.get(1)?, zone: taken_str.as_deref().and_then(tracks::zone_of), has_gps: r.get(3)? })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

async fn make_plan(state: &Arc<AppState>, b: &GeoBody) -> ApiResult<(Plan, Vec<Fix>, Vec<String>)> {
    if b.tracks.is_empty() {
        return Err(bad("add a track file first"));
    }
    if b.ids.is_empty() || b.ids.len() > MAX_PHOTOS {
        return Err(bad(format!("between 1 and {MAX_PHOTOS} photos")));
    }
    if b.tz.is_some_and(|z| !(-720..=840).contains(&z)) || b.drift.abs() > 7 * 86400 || !(10..=86400).contains(&b.gap) {
        return Err(bad("time zone, clock correction or gap out of range"));
    }
    let (fixes, names) = fixes_of(state, &b.tracks)?;
    let st = state.clone();
    let ids = b.ids.clone();
    let (tz, drift, gap, replace, prefer) = (b.tz, b.drift, b.gap * 1000, b.replace, b.prefer.clamp(-720, 840));
    let (plan, fixes) = tokio::task::spawn_blocking(move || -> anyhow::Result<(Plan, Vec<Fix>)> {
        let photos = photo_times(&st, &ids)?;
        Ok((plan(&fixes, &photos, tz, drift, gap, replace, prefer), fixes))
    })
    .await
    .map_err(|e| anyhow::anyhow!("{e}"))??;
    Ok((plan, fixes, names))
}

fn round(v: f64, digits: i32) -> f64 {
    let m = 10f64.powi(digits);
    (v * m).round() / m
}

async fn preview(State(state): St, Json(b): Json<GeoBody>) -> ApiResult<Json<Value>> {
    let (p, fixes, _) = make_plan(&state, &b).await?;
    let photos: Vec<Value> = p.matched.iter().map(|(id, f)| json!([id, round(f.lat, 6), round(f.lon, 6)])).collect();
    Ok(Json(json!({
        "tz": p.tz,
        "detected": p.plateau.is_some(),
        "plateau": p.plateau.map(|(a, b)| [a, b]),
        "counts": p.counts,
        "matched": p.matched.len(),
        "own_zone": p.own_zone,
        "no_time": p.no_time,
        "outside": p.outside,
        "has_gps": p.has_gps,
        "total": b.ids.len(),
        "photos": photos,
        "track": tracks::outline(&fixes, 6000, b.gap * 1000),
        "range": [fixes[0].t, fixes[fixes.len() - 1].t],
        "span": p.span.map(|(a, b)| [a, b]),
    })))
}

async fn apply(State(state): St, Json(b): Json<GeoBody>) -> ApiResult<Json<Value>> {
    check_writable(&state)?;
    let (p, _, names) = make_plan(&state, &b).await?;
    if p.matched.is_empty() {
        return Err(bad("no photo falls on the track"));
    }
    let n = p.matched.len();
    let from = if names.len() == 1 { names[0].clone() } else { format!("{} tracks", names.len()) };
    let label = format!("Geotagged {n} photo{} from {from}", if n == 1 { "" } else { "s" });
    let items = p
        .matched
        .into_iter()
        .map(|(id, f)| {
            let alt = f.ele.is_finite().then(|| round(f.ele as f64, 1));
            (id, Patch { gps: Some(Some(Gps { lat: round(f.lat, 7), lon: round(f.lon, 7), alt })), ..Default::default() })
        })
        .collect();
    let job = ops::spawn_patch_job(&state, label, items)?;
    Ok(Json(json!({ "job": job })))
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: i64 = 1_714_557_600; // 2024-05-01T10:00:00Z, seconds

    fn walk() -> Vec<Fix> {
        // Two hours, a fix a minute, heading north.
        (0..=120).map(|m| Fix { t: (T0 + m * 60) * 1000, lat: 45.0 + m as f64 * 0.001, lon: 7.0, ele: 500.0 + m as f32, stay: false }).collect()
    }

    fn photo(id: i64, utc: i64, zone_min: i32, own: bool, has_gps: bool) -> PhotoTime {
        PhotoTime { id, taken: Some(utc + zone_min as i64 * 60), zone: own.then_some(zone_min), has_gps }
    }

    #[test]
    fn planning() {
        let fixes = walk();
        let gap = 5 * 60_000;
        let photos = vec![
            photo(1, T0 + 600, 120, false, false),  // 10:10 UTC, camera on UTC+2
            photo(2, T0 + 3600, 120, false, false), // 11:00
            photo(3, T0 + 1800, -300, true, false), // its own zone (UTC-5) is used
            photo(4, T0 + 1200, 120, false, true),  // already has a position
            PhotoTime { id: 5, taken: None, zone: None, has_gps: false },
            photo(6, T0 + 3 * 3600, 120, false, false), // after the walk
        ];
        let p = plan(&fixes, &photos, Some(120), 0, gap, false, 0);
        assert_eq!(p.tz, 120);
        assert!(p.plateau.is_none());
        let got: Vec<(i64, f64)> = p.matched.iter().map(|(id, f)| (*id, (f.lat * 1000.0).round() / 1000.0)).collect();
        assert_eq!(got, vec![(1, 45.01), (2, 45.06), (3, 45.03)]);
        assert_eq!((p.own_zone, p.no_time, p.outside, p.has_gps), (1, 1, 1, 1));
        assert_eq!(p.matched[0].1.ele, 510.0);
        // Replacing takes in the photo that has a position.
        assert_eq!(plan(&fixes, &photos, Some(120), 0, gap, true, 0).matched.len(), 4);
        // A slow camera clock: two minutes behind.
        let slow: Vec<PhotoTime> = photos.iter().map(|x| PhotoTime { taken: x.taken.map(|t| t - 120), ..x.clone() }).collect();
        let p = plan(&fixes, &slow, Some(120), 120, gap, false, 0);
        assert_eq!(p.matched[0].1.lat, 45.01);
        // The zone is found when not given; the browser's zone loses to a better fit.
        let p = plan(&fixes, &photos, None, 0, gap, false, -300);
        assert_eq!(p.plateau, Some((60, 135)));
        assert_eq!(p.tz, 120);
        assert_eq!(p.counts.iter().find(|c| c.0 == 120).map(|c| c.1), Some(3));
        // Nothing on the track.
        let p = plan(&fixes, &[photo(9, T0 - 86400, 0, false, false)], Some(0), 0, gap, false, 0);
        assert_eq!((p.matched.len(), p.outside), (0, 1));
        assert_eq!(p.span, Some(((T0 - 86400) * 1000, (T0 - 86400) * 1000)));
    }
}
