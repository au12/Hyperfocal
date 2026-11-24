//! HTTP API of the Duplicates report.

use super::report::{self, Mode, Params, Scope};
use crate::api::{ApiResult, St, bad};
use crate::state::AppState;
use anyhow::Result;
use axum::extract::{Query, State};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/dupes", get(get_report))
        .route("/dupes/status", get(status))
        .route("/dupes/dismiss", post(dismiss))
        .route("/dupes/dismissed", delete(clear_dismissed))
}

#[derive(Deserialize, Default)]
pub struct ReportQuery {
    /// exact (default), near, series
    mode: Option<String>,
    /// library (default), folder, against
    scope: Option<String>,
    folder: Option<i64>,
    /// Near-duplicates: strict, normal (default), loose.
    level: Option<String>,
    /// Series: longest pause, seconds (default 2).
    gap: Option<i64>,
    /// Series: fewest frames (default 2).
    min: Option<usize>,
    limit: Option<usize>,
}

fn params(q: &ReportQuery) -> ApiResult<Params> {
    let mode = match q.mode.as_deref().unwrap_or("exact") {
        "exact" => Mode::Exact,
        "near" => Mode::Near,
        "series" => Mode::Series,
        "visual" => Mode::Visual,
        _ => return Err(bad("mode: exact, near, series or visual")),
    };
    let scope = match (q.scope.as_deref().unwrap_or("library"), q.folder) {
        ("library", _) => Scope::Library,
        ("folder", Some(f)) => Scope::Folder(f),
        ("against", Some(f)) => Scope::Against(f),
        ("folder" | "against", None) => return Err(bad("a folder is needed for that scope")),
        _ => return Err(bad("scope: library, folder or against")),
    };
    let t = crate::tuning::get();
    let radius = match q.level.as_deref().unwrap_or("normal") {
        "strict" => t.near_strict as u32,
        "normal" => t.near_normal as u32,
        "loose" => t.near_loose as u32,
        _ => return Err(bad("level: strict, normal or loose")),
    };
    let alike = match q.level.as_deref().unwrap_or("normal") {
        "strict" => t.alike_strict as f32,
        "loose" => t.alike_loose as f32,
        _ => t.alike_normal as f32,
    };
    // Series as culling finds them (Settings → Advanced); `gap` sets another longest pause.
    let mut series = t.series_rules();
    if let Some(g) = q.gap {
        series.max_gap = g.clamp(0, 900) as f64;
        series.burst_gap = series.burst_gap.min(series.max_gap);
    }
    if let Some(m) = q.min {
        series.min_len = m.clamp(2, 50) as usize;
    }
    Ok(Params { mode, scope, radius, alike, series, limit: q.limit.unwrap_or(2000).clamp(1, 5000) })
}

async fn get_report(State(state): St, Query(q): Query<ReportQuery>) -> ApiResult<Json<Value>> {
    let p = params(&q)?;
    let st = state.clone();
    let v = tokio::task::spawn_blocking(move || -> Result<Value> {
        let c = st.db.get()?;
        let t = std::time::Instant::now();
        // A folder asked about is hashed before the rest of the library.
        if matches!(p.mode, Mode::Near | Mode::Series) && st.cfg.dupes {
            if let Scope::Folder(f) | Scope::Against(f) = p.scope {
                st.dupes.prioritize(&report::unhashed_below(&c, f)?);
            }
        }
        let r = if matches!(p.mode, Mode::Visual | Mode::Series) {
            let ix = st.visual.index.read();
            report::build_with(&c, &p, Some(&ix))?
        } else {
            report::build(&c, &p)?
        };
        tracing::debug!("duplicates report ({:?}, {:?}): {} groups in {} ms", p.mode, p.scope, r.total_groups, t.elapsed().as_millis());
        let mut v = serde_json::to_value(&r)?;
        v["status"] = serde_json::to_value(&*st.dupes.status.read())?;
        v["integrity"] = json!(st.cfg.integrity);
        v["ms"] = json!(t.elapsed().as_millis() as u64);
        Ok(v)
    })
    .await
    .map_err(|e| anyhow::anyhow!("{e}"))??;
    Ok(Json(v))
}

async fn status(State(state): St) -> Json<Value> {
    Json(json!(*state.dupes.status.read()))
}

#[derive(Deserialize)]
struct DismissBody {
    ids: Vec<i64>,
    /// Take a dismissal back (Undo).
    #[serde(default)]
    undo: bool,
}

/// "Not duplicates": these photos are never grouped together again.
async fn dismiss(State(state): St, Json(b): Json<DismissBody>) -> ApiResult<Json<Value>> {
    let mut ids = b.ids;
    ids.sort_unstable();
    ids.dedup();
    if ids.len() < 2 || ids.len() > 500 {
        return Err(bad("between 2 and 500 photos"));
    }
    let st = state.clone();
    let n = tokio::task::spawn_blocking(move || -> Result<usize> {
        let mut c = st.db.get()?;
        let tx = c.transaction()?;
        let now = crate::db::now_ms();
        let mut n = 0;
        {
            let mut add = tx.prepare("INSERT OR IGNORE INTO dupes_ignored(a, b, at) SELECT ?1, ?2, ?3 WHERE EXISTS(SELECT 1 FROM photos WHERE id = ?1) AND EXISTS(SELECT 1 FROM photos WHERE id = ?2)")?;
            let mut del = tx.prepare("DELETE FROM dupes_ignored WHERE a = ?1 AND b = ?2")?;
            for i in 0..ids.len() {
                for j in i + 1..ids.len() {
                    n += if b.undo { del.execute([ids[i], ids[j]])? } else { add.execute([ids[i], ids[j], now])? };
                }
            }
        }
        tx.commit()?;
        Ok(n)
    })
    .await
    .map_err(|e| anyhow::anyhow!("{e}"))??;
    Ok(Json(json!({ "pairs": n })))
}

/// Show every dismissed group again.
async fn clear_dismissed(State(state): St) -> ApiResult<Json<Value>> {
    let n = state.db.get()?.execute("DELETE FROM dupes_ignored", [])?;
    Ok(Json(json!({ "cleared": n })))
}
