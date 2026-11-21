//! Culling help: per photo, how sharp its subject is (the faces, else the animals, else
//! the sharpest part of the photo, measured on the RAW development: `focus`), whether
//! someone's eyes are closed (the ML service), and clipped highlights or a dark exposure
//! (the RAW data). Shown as badges, "possible rejects" filters and the suggested best
//! frame of a series: nothing is ever rejected automatically.

pub mod api;
pub mod focus;
pub mod measure;
pub mod series;
pub mod worker;

use crate::state::AppState;
use anyhow::Result;
use parking_lot::{Condvar, Mutex, RwLock};
use rusqlite::Connection;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::VecDeque;

/// How photos are measured: a new version measures every photo again.
pub const VERSION: i64 = 1;

/// Sharpness (0..1, `focus::score`) below which a subject is blurry.
pub const BLURRY: f64 = 0.3;
/// Sharpness from which a subject is sharp.
pub const SHARP: f64 = 0.7;
/// Eyes count as closed from this score (0 open .. 1 closed, see ml/hyperfocal_ml/eyes.py).
pub const CLOSED: f64 = 0.45;
/// Highlights are clipped when this share of the pixels has a clipped channel.
pub const CLIPPED_AT: f64 = 0.015;
/// Too dark when this share of the pixels is underexposed (the RAW's own threshold, about
/// 7.7 stops below white).
pub const DARK_AT: f64 = 0.5;

/// What may be wrong with a photo, as bits.
pub const ISSUE_BLUR: i64 = 1;
pub const ISSUE_EYES: i64 = 2;
pub const ISSUE_CLIPPED: i64 = 4;
pub const ISSUE_DARK: i64 = 8;

/// Faces (and animals) smaller than this, long side in pixels of the measured image, are
/// too small to judge focus or eyes on.
pub const SUBJECT_MIN_PX: f64 = 64.0;
/// A face counts as one of the people in a photo from this height (share of the photo's
/// height): smaller ones are passers-by and crowds.
pub const PERSON_MIN_H: f64 = 0.025;
/// A portrait's one face is at least this tall.
pub const PORTRAIT_MIN_H: f64 = 0.08;
/// A group: this many people or more.
pub const GROUP_MIN: i64 = 3;
/// A face People & pets found that is surely a face (SQL on `detections`): the detector
/// was sure of it, or it is clear enough to recognise, or someone named or confirmed it,
/// or another app marked it. Shapes the detector half-took for a face (a sculpture, a
/// pattern) are left out of culling and of the people filters.
pub const REAL_FACE: &str = "(score >= 0.8 OR quality >= 0.15 OR state = 1 OR source = 1)";

pub fn migrate(c: &Connection) -> Result<()> {
    c.execute_batch(
        r#"
        -- What culling measured in a photo, for the file as it was (size, mtime,
        -- orientation) and the faces of the people analysis at `faces_at`.
        CREATE TABLE IF NOT EXISTS cull(
            photo_id INTEGER PRIMARY KEY REFERENCES photos(id) ON DELETE CASCADE,
            size INTEGER NOT NULL,
            mtime INTEGER NOT NULL,
            orientation INTEGER NOT NULL,
            faces_at INTEGER NOT NULL DEFAULT 0,
            at INTEGER NOT NULL,
            error TEXT,
            -- Sharpness of the subject, 0..1 (1 sharp), and what it was measured on:
            -- 1 faces, 2 animals, 3 the sharpest part of the photo.
            focus REAL,
            focus_on INTEGER,
            -- Shares of the pixels with a clipped channel, and underexposed.
            over REAL,
            under REAL,
            -- Faces whose eyes were looked at (NULL: no eye checks), and how many closed.
            eyes INTEGER,
            closed INTEGER,
            -- ISSUE_* bits.
            issues INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS cull_issues ON cull(issues) WHERE issues != 0;

        -- Per face (or animal): sharpness, eyes (0 open .. 1 closed) and where the eyes
        -- are on the upright photo ([[x, y], [x, y]], 0..1), for close-ups.
        CREATE TABLE IF NOT EXISTS cull_faces(
            detection_id INTEGER PRIMARY KEY REFERENCES detections(id) ON DELETE CASCADE,
            photo_id INTEGER NOT NULL REFERENCES photos(id) ON DELETE CASCADE,
            focus REAL,
            closed REAL,
            eyes TEXT
        );
        CREATE INDEX IF NOT EXISTS cull_faces_photo ON cull_faces(photo_id);

        -- People filters count faces per photo: from the index alone.
        DROP INDEX IF EXISTS detections_people;
        CREATE INDEX IF NOT EXISTS detections_faces ON detections(kind, photo_id, h, state, score, quality, source);
        "#,
    )?;
    if crate::db::get_meta(c, "cull_version").and_then(|v| v.parse::<i64>().ok()) != Some(VERSION) {
        c.execute_batch("DELETE FROM cull_faces; DELETE FROM cull;")?;
        crate::db::set_meta(c, "cull_version", &VERSION.to_string())?;
    }
    Ok(())
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Status {
    pub phase: String,
    pub done: i64,
    pub total: i64,
    /// Photos a minute, over the last few minutes.
    pub rate: f64,
    pub error: Option<String>,
    /// Eyes are checked (the ML service has the models).
    pub eyes: bool,
}

#[derive(Default)]
pub struct Ctl {
    pub wake: bool,
    pub priority: VecDeque<i64>,
}

pub struct Cull {
    pub status: RwLock<Status>,
    pub ctl: Mutex<Ctl>,
    pub cv: Condvar,
}

impl Default for Cull {
    fn default() -> Cull {
        Cull::new()
    }
}

impl Cull {
    pub fn new() -> Cull {
        Cull { status: RwLock::new(Status { phase: "starting".into(), ..Default::default() }), ctl: Mutex::new(Ctl::default()), cv: Condvar::new() }
    }

    pub fn wake(&self) {
        self.ctl.lock().wake = true;
        self.cv.notify_all();
    }

    /// Measure these photos next (the ones on screen).
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

pub fn status_json(state: &AppState) -> Value {
    let s = state.cull.status.read().clone();
    json!({
        "phase": s.phase,
        "done": s.done,
        "total": s.total,
        "rate": (s.rate * 10.0).round() / 10.0,
        "error": s.error,
        "eyes": s.eyes,
    })
}

/// The issues of a photo from its measurements.
pub fn issues(focus: Option<f64>, closed: Option<i64>, over: Option<f64>, under: Option<f64>) -> i64 {
    let mut i = 0;
    if focus.is_some_and(|f| f < BLURRY) {
        i |= ISSUE_BLUR;
    }
    if closed.is_some_and(|n| n > 0) {
        i |= ISSUE_EYES;
    }
    if over.is_some_and(|o| o >= CLIPPED_AT) {
        i |= ISSUE_CLIPPED;
    }
    if under.is_some_and(|u| u >= DARK_AT) {
        i |= ISSUE_DARK;
    }
    i
}

/// Names of issue bits, for the API.
pub fn issue_names(bits: i64) -> Vec<&'static str> {
    let mut v = Vec::new();
    for (b, n) in [(ISSUE_BLUR, "blur"), (ISSUE_EYES, "eyes"), (ISSUE_CLIPPED, "over"), (ISSUE_DARK, "under")] {
        if bits & b != 0 {
            v.push(n);
        }
    }
    v
}

/// Issue bits from names ("blur,eyes,over,under").
pub fn issue_bits(names: &str) -> i64 {
    names
        .split(',')
        .map(|n| match n.trim() {
            "blur" => ISSUE_BLUR,
            "eyes" => ISSUE_EYES,
            "over" => ISSUE_CLIPPED,
            "under" => ISSUE_DARK,
            _ => 0,
        })
        .fold(0, |a, b| a | b)
}

#[cfg(test)]
mod tests {
    use crate::images::jpeg::{self, Rgb};
    use crate::model::PhotoRec;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use serde_json::Value;
    use tower::ServiceExt;

    /// A busy scene: blocks, discs and stripes in every direction.
    fn scene(w: usize, h: usize, seed: u64) -> Rgb {
        let mut x = seed | 1;
        let mut rnd = move |n: usize| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x % n as u64) as usize
        };
        let mut px = vec![90u8; w * h * 3];
        for _ in 0..60 {
            let (cx, cy, r) = (rnd(w), rnd(h), 10 + rnd(60));
            let c = [rnd(200) as u8 + 30, rnd(200) as u8 + 30, rnd(200) as u8 + 30];
            let stripes = rnd(3) == 0;
            for y in cy.saturating_sub(r)..(cy + r).min(h) {
                for xx in cx.saturating_sub(r)..(cx + r).min(w) {
                    let inside = if rnd(2) == 0 { true } else { (xx as i64 - cx as i64).pow(2) + (y as i64 - cy as i64).pow(2) < (r * r) as i64 };
                    if inside && (!stripes || ((xx + y) / 5) % 2 == 0) {
                        px[(y * w + xx) * 3..(y * w + xx) * 3 + 3].copy_from_slice(&c);
                    }
                }
            }
        }
        Rgb { w, h, px }
    }

    fn blur(img: &Rgb, sigma: f32) -> Rgb {
        let r = (sigma * 3.0).ceil() as i64;
        let k: Vec<f32> = (-r..=r).map(|i| (-(i * i) as f32 / (2.0 * sigma * sigma)).exp()).collect();
        let s: f32 = k.iter().sum();
        let (w, h) = (img.w as i64, img.h as i64);
        let pass = |src: &[u8], horizontal: bool| -> Vec<u8> {
            let mut out = vec![0u8; src.len()];
            for y in 0..h {
                for x in 0..w {
                    for c in 0..3 {
                        let mut acc = 0.0;
                        for (j, kv) in k.iter().enumerate() {
                            let d = j as i64 - r;
                            let (sx, sy) = if horizontal { ((x + d).clamp(0, w - 1), y) } else { (x, (y + d).clamp(0, h - 1)) };
                            acc += kv * src[((sy * w + sx) * 3 + c) as usize] as f32;
                        }
                        out[((y * w + x) * 3 + c) as usize] = (acc / s).round() as u8;
                    }
                }
            }
            out
        };
        let a = pass(&img.px, true);
        Rgb { w: img.w, h: img.h, px: pass(&a, false) }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn series_stack_on_their_best_frame_and_rejects_are_found() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("photos").join("Shoot");
        std::fs::create_dir_all(&dir).unwrap();
        let sharp = scene(900, 600, 7);
        let mut blown = scene(900, 600, 7);
        for p in blown.px.chunks_exact_mut(3).take(900 * 150) {
            p.copy_from_slice(&[255, 255, 255]);
        }
        for (name, img) in [("A1.JPG", &sharp), ("A2.JPG", &blur(&sharp, 3.5)), ("A3.JPG", &blown), ("B1.JPG", &scene(900, 600, 11))] {
            std::fs::write(dir.join(name), jpeg::encode(img, 92, true).unwrap()).unwrap();
        }
        let cfg = crate::config::Config::for_tests(tmp.path());
        let state = crate::build_state(cfg).await.unwrap();
        assert!(crate::scan::sync_tree(&state).unwrap());
        let folder: i64 = state.db.get().unwrap().query_row("SELECT id FROM folders WHERE path = 'Shoot'", [], |r| r.get(0)).unwrap();
        crate::scan::scan_folder(&state, folder, true).unwrap();
        let ids: Vec<(i64, String)> = {
            let c = state.db.get().unwrap();
            // One camera, the A frames a second apart, B a minute later.
            c.execute_batch(
                "UPDATE photos SET make = 'Canon', model = 'EOS R6',
                    taken = 1700000000 + CASE name WHEN 'A1.JPG' THEN 0 WHEN 'A2.JPG' THEN 1 WHEN 'A3.JPG' THEN 1 ELSE 60 END",
            )
            .unwrap();
            let mut st = c.prepare("SELECT id, name FROM photos ORDER BY name").unwrap();
            st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(|r| r.unwrap()).collect()
        };
        let id = |n: &str| ids.iter().find(|(_, m)| m == n).unwrap().0;
        let lut = crate::images::tone::build_lut(crate::images::tone::ToneMode::Normal, crate::images::RENDER_BOOST_EV);
        let mode = super::worker::Mode { eyes: false, wait_for_people: false };
        for (pid, _) in &ids {
            let rec = {
                let c = state.db.get().unwrap();
                c.query_row("SELECT id, size, mtime, orientation FROM photos WHERE id = ?1", [pid], |r| {
                    Ok(PhotoRec { id: r.get(0)?, size: r.get(1)?, mtime: r.get(2)?, orientation: r.get(3)?, ..Default::default() })
                })
                .unwrap()
            };
            let st = state.clone();
            let lut = lut.clone();
            assert!(tokio::task::spawn_blocking(move || super::worker::one(&st, None, mode, &lut, &rec).unwrap()).await.unwrap());
        }
        let (focus, issues): (Vec<f64>, Vec<i64>) = {
            let c = state.db.get().unwrap();
            let mut st = c.prepare("SELECT focus, issues FROM cull c JOIN photos p ON p.id = c.photo_id ORDER BY p.name").unwrap();
            st.query_map([], |r| Ok((r.get::<_, f64>(0)?, r.get::<_, i64>(1)?))).unwrap().map(|r| r.unwrap()).unzip()
        };
        assert!(focus[0] > 0.6 && focus[1] < super::BLURRY && focus[2] > 0.6, "{focus:?}");
        assert_eq!(issues, vec![0, super::ISSUE_BLUR, super::ISSUE_CLIPPED, 0]);

        let app = crate::api::router(state.clone());
        let get = |uri: String| {
            let app = app.clone();
            async move {
                let resp = app.oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap()).await.unwrap();
                assert_eq!(resp.status(), StatusCode::OK);
                let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
                serde_json::from_slice::<Value>(&bytes).unwrap()
            }
        };
        // Folded: the series shows as its best frame (sharp, the first of the two sharp
        // ones), carrying the count.
        let l = get(format!("/api/photos?folder={folder}&cull=1&stack=1")).await;
        assert_eq!(l["id"], serde_json::json!([id("A1.JPG"), id("B1.JPG")]), "{l}");
        assert_eq!(l["sn"], serde_json::json!([3, 0]));
        assert_eq!(l["sb"], serde_json::json!([1, 0]));
        assert_eq!(l["cq"], serde_json::json!([0, 0]));
        let key = l["sk"][0].as_i64().unwrap();
        assert_eq!(key, id("A1.JPG"));
        // Unfolded: every frame, in shooting order.
        let l = get(format!("/api/photos?folder={folder}&cull=1&stack=1&open={key}")).await;
        assert_eq!(l["id"].as_array().unwrap().len(), 4);
        assert_eq!(l["sb"], serde_json::json!([1, 0, 0, 0]));
        // The frames that are not their series' best; the blurry ones.
        let l = get(format!("/api/photos?folder={folder}&weak=1")).await;
        assert_eq!(l["id"], serde_json::json!([id("A2.JPG"), id("A3.JPG")]));
        let l = get(format!("/api/photos?folder={folder}&issue=blur")).await;
        assert_eq!(l["id"], serde_json::json!([id("A2.JPG")]));
        let n = get(format!("/api/photos?folder={folder}&weak=1&count=1")).await;
        assert_eq!(n["count"], 2);
        // Sharpest first.
        let l = get(format!("/api/photos?folder={folder}&sort=focus")).await;
        assert_eq!(l["id"].as_array().unwrap().last().unwrap().as_i64(), Some(id("A2.JPG")));
        // A frame's series, for the single view.
        let p = get(format!("/api/cull/photo/{}", id("A2.JPG"))).await;
        assert_eq!(p["series"]["best"].as_i64(), Some(id("A1.JPG")));
        assert_eq!(p["series"]["frames"].as_array().unwrap().len(), 3);
        assert_eq!(p["issues"], serde_json::json!(["blur"]));
        // People: a portrait (one face, close), a group of three (one blinking), a
        // detector's mistake (a shape it half-took for a face) that counts for nothing.
        {
            let c = state.db.get().unwrap();
            for (pid, _) in &ids {
                c.execute("INSERT INTO analysis(photo_id, size, mtime, orientation, models, at) SELECT id, size, mtime, orientation, 'm', 1 FROM photos WHERE id = ?1", [pid]).unwrap();
            }
            let det = |photo: i64, x: f64, h: f64, score: f64, quality: f64| -> i64 {
                c.query_row(
                    "INSERT INTO detections(photo_id, kind, x, y, w, h, score, quality, created) VALUES(?1, 1, ?2, 0.2, ?3, ?3, ?4, ?5, 0) RETURNING id",
                    rusqlite::params![photo, x, h, score, quality],
                    |r| r.get(0),
                )
                .unwrap()
            };
            det(id("B1.JPG"), 0.4, 0.3, 0.95, 0.9);
            det(id("B1.JPG"), 0.1, 0.4, 0.72, 0.03);
            let blinker = det(id("A1.JPG"), 0.1, 0.06, 0.9, 0.8);
            det(id("A1.JPG"), 0.4, 0.06, 0.9, 0.8);
            det(id("A1.JPG"), 0.7, 0.06, 0.9, 0.8);
            c.execute("INSERT INTO cull_faces(detection_id, photo_id, focus, closed) VALUES(?1, ?2, 0.9, 0.8)", [blinker, id("A1.JPG")]).unwrap();
        }
        let l = get(format!("/api/photos?folder={folder}&shot=portrait")).await;
        assert_eq!(l["id"], serde_json::json!([id("B1.JPG")]));
        let l = get(format!("/api/photos?folder={folder}&shot=group")).await;
        assert_eq!(l["id"], serde_json::json!([id("A1.JPG")]));
        let l = get(format!("/api/photos?folder={folder}&ppl=1")).await;
        assert_eq!(l["id"], serde_json::json!([id("B1.JPG")]));
        let l = get(format!("/api/photos?folder={folder}&ppl=3-")).await;
        assert_eq!(l["id"], serde_json::json!([id("A1.JPG")]));
        let l = get(format!("/api/photos?folder={folder}&ppl=0")).await;
        assert_eq!(l["id"], serde_json::json!([id("A2.JPG"), id("A3.JPG")]));
        let l = get(format!("/api/photos?folder={folder}&eyes=closed")).await;
        assert_eq!(l["id"], serde_json::json!([id("A1.JPG")]));
        // A rating overrules the measurements.
        state.db.get().unwrap().execute("UPDATE photos SET rating = 4 WHERE name = 'A3.JPG'", []).unwrap();
        let l = get(format!("/api/photos?folder={folder}&cull=1&stack=1")).await;
        assert_eq!(l["id"][0].as_i64(), Some(id("A3.JPG")));
    }
}
