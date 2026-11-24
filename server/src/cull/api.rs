//! The culling API: progress, a photo's measurements with its series (each face followed
//! across the frames, for close-ups side by side), and the photos to measure first.

use super::series::{self, Frame};
use super::PERSON_MIN_H;
use crate::api::{ApiError, ApiResult, St, bad};
use crate::model::PhotoRec;
use crate::people::regions::Rect;
use crate::state::AppState;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use rusqlite::{Connection, params_from_iter};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/cull/status", get(status))
        .route("/cull/photo/{id}", get(photo))
        .route("/cull/measure", post(measure_first))
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> anyhow::Result<T> + Send + 'static) -> ApiResult<T> {
    tokio::task::spawn_blocking(f).await.map_err(|e| anyhow::anyhow!("{e}"))?.map_err(ApiError::from)
}

async fn status(State(state): St) -> Json<Value> {
    Json(super::status_json(&state))
}

#[derive(Deserialize)]
struct Ids {
    ids: Vec<i64>,
}

/// Measure these photos next (the viewer asks for the ones on screen).
async fn measure_first(State(state): St, Json(b): Json<Ids>) -> ApiResult<Json<Value>> {
    if b.ids.len() > 2000 {
        return Err(bad("at most 2000 photos"));
    }
    state.cull.prioritize(&b.ids);
    Ok(Json(json!({ "ok": true })))
}

/// How far around a photo its series is looked for.
const AROUND_S: i64 = 600;

struct Row {
    frame: Frame,
    rec: PhotoRec,
    w: Option<i64>,
    h: Option<i64>,
    issues: Option<i64>,
    focus_on: Option<i64>,
    over: Option<f64>,
    under: Option<f64>,
    eyes: Option<i64>,
    error: Option<String>,
    measured: bool,
}

#[derive(Clone)]
struct Face {
    id: i64,
    photo: i64,
    rect: Rect,
    subject: Option<i64>,
    name: Option<String>,
    focus: Option<f64>,
    closed: Option<f64>,
    eyes: Option<Value>,
}

fn rows(c: &Connection, wh: &str, args: &[i64]) -> anyhow::Result<Vec<Row>> {
    let sql = format!(
        "SELECT p.id, p.name, p.folder_id, COALESCE(p.make, '') || ' ' || COALESCE(p.model, ''), p.taken, p.rating, p.pick,
                p.size, p.mtime, p.orientation, p.width, p.height, h.hash, h.sharp,
                c.focus, c.closed, c.issues, c.focus_on, c.over, c.under, c.eyes, c.error, c.photo_id IS NOT NULL
         FROM photos p JOIN folders f ON f.id = p.folder_id LEFT JOIN phashes h ON h.photo_id = p.id
         LEFT JOIN cull c ON c.photo_id = p.id AND c.size = p.size AND c.mtime = p.mtime AND c.orientation = p.orientation
         WHERE {wh}"
    );
    let mut st = c.prepare(&sql)?;
    let out = st
        .query_map(params_from_iter(args.iter()), |r| {
            let id: i64 = r.get(0)?;
            let rec = PhotoRec { id, size: r.get(7)?, mtime: r.get(8)?, orientation: r.get(9)?, ..Default::default() };
            Ok(Row {
                frame: Frame {
                    id,
                    name: r.get(1)?,
                    folder: r.get(2)?,
                    camera: r.get(3)?,
                    taken: r.get(4)?,
                    rating: r.get(5)?,
                    pick: r.get(6)?,
                    hash: r.get::<_, Option<i64>>(12)?.map(|h| h as u64),
                    sharp: r.get(13)?,
                    focus: r.get(14)?,
                    closed: r.get(15)?,
                    emb: None,
                },
                rec,
                w: r.get(10)?,
                h: r.get(11)?,
                issues: r.get(16)?,
                focus_on: r.get(17)?,
                over: r.get(18)?,
                under: r.get(19)?,
                eyes: r.get(20)?,
                error: r.get(21)?,
                measured: r.get(22)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(out)
}

fn faces(c: &Connection, photos: &[i64]) -> anyhow::Result<Vec<Face>> {
    if photos.is_empty() {
        return Ok(Vec::new());
    }
    let marks = vec!["?"; photos.len()].join(",");
    let sql = format!(
        "SELECT d.id, d.photo_id, d.x, d.y, d.w, d.h, d.subject_id, s.name, cf.focus, cf.closed, cf.eyes
         FROM detections d LEFT JOIN subjects s ON s.id = d.subject_id LEFT JOIN cull_faces cf ON cf.detection_id = d.id
         WHERE d.photo_id IN ({marks}) AND d.kind = 1 AND d.state != 3 AND {} ORDER BY d.x",
        super::REAL_FACE.replace("score", "d.score").replace("quality", "d.quality").replace("state", "d.state").replace("source", "d.source")
    );
    let mut st = c.prepare(&sql)?;
    let out = st
        .query_map(params_from_iter(photos.iter()), |r| {
            Ok(Face {
                id: r.get(0)?,
                photo: r.get(1)?,
                rect: Rect { x: r.get(2)?, y: r.get(3)?, w: r.get(4)?, h: r.get(5)? },
                subject: r.get(6)?,
                name: r.get(7)?,
                focus: r.get(8)?,
                closed: r.get(9)?,
                eyes: r.get::<_, Option<String>>(10)?.and_then(|s| serde_json::from_str(&s).ok()),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(out)
}

fn iou(a: Rect, b: Rect) -> f64 {
    let iw = ((a.x + a.w).min(b.x + b.w) - a.x.max(b.x)).max(0.0);
    let ih = ((a.y + a.h).min(b.y + b.h) - a.y.max(b.y)).max(0.0);
    let i = iw * ih;
    let u = a.w * a.h + b.w * b.h - i;
    if u > 0.0 { i / u } else { 0.0 }
}

fn face_json(f: &Face, photo_v: &str) -> Value {
    json!({
        "id": f.id,
        "photo": f.photo,
        "box": [f.rect.x, f.rect.y, f.rect.w, f.rect.h],
        "focus": f.focus,
        "closed": f.closed,
        "shut": f.closed.map(|c| c >= super::thresholds().closed),
        "eyes": f.eyes,
        "subject": f.subject.map(|id| json!({ "id": id, "name": f.name })),
        "v": crate::people::api::crop_v(&f.rect, photo_v),
    })
}

fn on_name(on: Option<i64>) -> Option<&'static str> {
    match on {
        Some(1) => Some("faces"),
        Some(2) => Some("animals"),
        Some(3) => Some("photo"),
        _ => None,
    }
}

/// Follows each face of `frames[at]` through the other frames: the same person, else the
/// face in about the same place. Returns per face the matching face of every frame.
fn tracks(frames: &[i64], at: usize, by_photo: &HashMap<i64, Vec<Face>>) -> Vec<Vec<Option<Face>>> {
    let empty = Vec::new();
    let own = by_photo.get(&frames[at]).unwrap_or(&empty);
    // The people in the photo: no passers-by.
    let own: Vec<&Face> = own.iter().filter(|f| f.rect.h >= PERSON_MIN_H || f.focus.is_some() || f.closed.is_some()).collect();
    let mut out: Vec<Vec<Option<Face>>> = own.iter().map(|_| vec![None; frames.len()]).collect();
    for (k, &pid) in frames.iter().enumerate() {
        let theirs = by_photo.get(&pid).unwrap_or(&empty);
        if k == at {
            for (t, f) in own.iter().enumerate() {
                out[t][k] = Some((*f).clone());
            }
            continue;
        }
        // Best matches first, each face of the frame used once.
        let mut pairs: Vec<(f64, usize, usize)> = Vec::new();
        for (t, f) in own.iter().enumerate() {
            for (j, g) in theirs.iter().enumerate() {
                let same = matches!((f.subject, g.subject), (Some(a), Some(b)) if a == b);
                let o = iou(f.rect, g.rect);
                let score = if same { 1.0 + o } else { o };
                if same || o >= 0.3 {
                    pairs.push((score, t, j));
                }
            }
        }
        pairs.sort_by(|a, b| b.0.total_cmp(&a.0));
        let (mut used_t, mut used_j) = (HashSet::new(), HashSet::new());
        for (_, t, j) in pairs {
            if used_t.contains(&t) || used_j.contains(&j) {
                continue;
            }
            used_t.insert(t);
            used_j.insert(j);
            out[t][k] = Some(theirs[j].clone());
        }
    }
    out
}

/// A photo's measurements, faces and series.
async fn photo(State(state): St, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let st = state.clone();
    let v = blocking(move || -> anyhow::Result<Option<Value>> {
        let c = st.db.get()?;
        let Some(me) = rows(&c, "p.id = ?1", &[id])?.pop() else { return Ok(None) };
        let tuning = crate::tuning::get();
        let rules = tuning.series_rules();
        // Its series: the frames of its folder (and camera) around it. The window widens
        // while the series reaches its edge (a long session).
        let (folder, camera) = (me.frame.folder, me.frame.camera.clone());
        let mut window = AROUND_S.max((rules.max_gap * 4.0) as i64);
        let (mut around, mut frames, mut series): (Vec<Row>, Vec<Frame>, Option<Vec<usize>>);
        loop {
            around = match me.frame.taken {
                Some(t) => rows(
                    &c,
                    "p.folder_id = ?1 AND p.pair_of IS NULL AND f.is_reject = 0 AND p.taken BETWEEN ?2 AND ?3 AND p.id != ?4",
                    &[folder, t - window, t + window, id],
                )?
                .into_iter()
                .filter(|r| rules.cameras || r.frame.camera == camera)
                .collect(),
                None => Vec::new(),
            };
            around.push(rows(&c, "p.id = ?1", &[id])?.pop().ok_or_else(|| anyhow::anyhow!("the photo went away"))?);
            {
                let ix = st.visual.index.read();
                for r in &mut around {
                    r.frame.emb = ix.slot(r.frame.id);
                }
                frames = around.iter().map(|r| r.frame.clone()).collect();
                let looks: Option<&dyn crate::dupes::bursts::Looks> = if ix.is_empty() { None } else { Some(&*ix) };
                let at = frames.len() - 1;
                series = series::group(&frames, rules, looks).into_iter().find(|g| g.contains(&at));
            }
            let (Some(t), Some(g)) = (me.frame.taken, &series) else { break };
            let first = frames[g[0]].taken.unwrap_or(t);
            let last = frames[*g.last().unwrap()].taken.unwrap_or(t);
            let near_edge = (first - (t - window)) as f64 <= rules.max_gap || ((t + window) - last) as f64 <= rules.max_gap;
            if !near_edge || window >= 2 * 86_400 {
                break;
            }
            window *= 4;
        }
        let at = frames.len() - 1;
        let shown: Vec<usize> = series.clone().unwrap_or_else(|| vec![at]);
        let ids: Vec<i64> = shown.iter().map(|&i| frames[i].id).collect();
        let all = faces(&c, &ids)?;
        let mut by_photo: HashMap<i64, Vec<Face>> = HashMap::new();
        for f in all {
            by_photo.entry(f.photo).or_default().push(f);
        }
        let sig = |r: &Row| crate::images::sig_str(&r.rec);
        let me_row = &around[at];
        let pending = !me_row.measured;
        let own: Vec<Value> = by_photo.get(&id).map(|v| v.iter().map(|f| face_json(f, &sig(me_row))).collect()).unwrap_or_default();
        let series_json = series.map(|g| {
            let choice = series::Choice::current();
            let best = series::best_with(&frames, &g, choice);
            let second = g.iter().copied().filter(|&i| i != best).max_by(|&a, &b| series::compare_with(&frames[a], &frames[b], choice));
            let k = g.iter().position(|&i| i == at).unwrap_or(0);
            let fids: Vec<i64> = g.iter().map(|&i| frames[i].id).collect();
            let sigs: HashMap<i64, String> = g.iter().map(|&i| (frames[i].id, sig(&around[i]))).collect();
            let tracks: Vec<Value> = tracks(&fids, k, &by_photo)
                .into_iter()
                .map(|t| {
                    let who = t.iter().flatten().find(|f| f.subject.is_some()).map(|f| json!({ "id": f.subject, "name": f.name }));
                    json!({
                        "subject": who,
                        "faces": t.iter().map(|f| f.as_ref().map(|f| face_json(f, &sigs[&f.photo]))).collect::<Vec<_>>(),
                    })
                })
                .collect();
            json!({
                "key": frames[g[0]].id,
                "best": frames[best].id,
                "why": second.map(|s| series::why(&frames[best], &frames[s], best == g[0])),
                "index": k,
                "frames": g.iter().map(|&i| {
                    let r = &around[i];
                    json!({
                        "id": r.frame.id,
                        "name": r.frame.name,
                        "v": sig(r),
                        "w": r.w, "h": r.h, "o": r.rec.orientation,
                        "rating": r.frame.rating,
                        "pick": r.frame.pick,
                        "focus": r.frame.focus,
                        "closed": r.frame.closed,
                        "issues": r.issues.map(super::issue_names).unwrap_or_default(),
                    })
                }).collect::<Vec<_>>(),
                "tracks": tracks,
            })
        });
        Ok(Some(json!({
            "id": id,
            "measured": me_row.measured && me_row.error.is_none(),
            "pending": pending,
            "error": me_row.error,
            "focus": me_row.frame.focus,
            "focus_on": on_name(me_row.focus_on),
            "over": me_row.over,
            "under": me_row.under,
            "eyes": me_row.eyes.map(|n| json!({ "checked": n, "closed": me_row.frame.closed })),
            "issues": me_row.issues.map(super::issue_names).unwrap_or_default(),
            "faces": own,
            "series": series_json,
        })))
    })
    .await?;
    v.map(Json).ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "not found".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face(id: i64, photo: i64, x: f64, subject: Option<i64>) -> Face {
        Face { id, photo, rect: Rect { x, y: 0.2, w: 0.1, h: 0.15 }, subject, name: None, focus: Some(0.8), closed: None, eyes: None }
    }

    #[test]
    fn faces_are_followed_by_person_then_by_place() {
        let mut by: HashMap<i64, Vec<Face>> = HashMap::new();
        by.insert(1, vec![face(10, 1, 0.1, Some(7)), face(11, 1, 0.5, None)]);
        // The next frame: the two stood still, but the person known by name moved.
        by.insert(2, vec![face(20, 2, 0.52, None), face(21, 2, 0.7, Some(7))]);
        // A frame where only the unnamed one is found.
        by.insert(3, vec![face(30, 3, 0.49, None)]);
        let t = tracks(&[1, 2, 3], 0, &by);
        let ids: Vec<Vec<Option<i64>>> = t.iter().map(|tr| tr.iter().map(|f| f.as_ref().map(|f| f.id)).collect()).collect();
        assert_eq!(ids, vec![vec![Some(10), Some(21), None], vec![Some(11), Some(20), Some(30)]]);
    }
}
