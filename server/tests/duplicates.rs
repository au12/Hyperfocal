//! The Duplicates report end to end: exact copies from the integrity hashes (a copy
//! that differs only in its metadata included), near-duplicates from the perceptual
//! hashes, series, the three scopes, "not duplicates", and the grid's `ids` filter.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use hyperfocal::images::jpeg;
use serde_json::{Value, json};
use tower::ServiceExt;

/// A synthetic photo, different for each seed: a gradient with hard-edged shapes.
fn scene(w: usize, h: usize, seed: u32) -> jpeg::Rgb {
    let mut x = seed.wrapping_mul(2_654_435_761) | 1;
    let mut rnd = move || {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        (x % 10_000) as f32 / 10_000.0
    };
    let shapes: Vec<[f32; 6]> = (0..12).map(|_| [rnd(), rnd(), 0.05 + 0.2 * rnd(), 0.05 + 0.2 * rnd(), rnd() * 255.0, rnd()]).collect();
    let (gx, gy) = (rnd() * 120.0 - 60.0, rnd() * 120.0 - 60.0);
    let mut px = Vec::with_capacity(w * h * 3);
    for yy in 0..h {
        for xx in 0..w {
            let (u, v) = (xx as f32 / w as f32, yy as f32 / h as f32);
            let mut l = 128.0 + gx * (u - 0.5) + gy * (v - 0.5);
            for s in &shapes {
                let (dx, dy) = ((u - s[0]) / s[2], (v - s[1]) / s[3]);
                if if s[5] < 0.5 { dx * dx + dy * dy < 1.0 } else { dx.abs() < 1.0 && dy.abs() < 1.0 } {
                    l = 0.3 * l + 0.7 * s[4];
                }
            }
            px.extend_from_slice(&[(l + 12.0 * u).clamp(0.0, 255.0) as u8, l.clamp(0.0, 255.0) as u8, (l - 18.0 * v).clamp(0.0, 255.0) as u8]);
        }
    }
    jpeg::Rgb { w, h, px }
}

/// The same JPEG with a comment segment after the start marker: other metadata, the
/// same image data (as when another app writes its metadata into the file).
fn with_comment(data: &[u8]) -> Vec<u8> {
    let text = b"Edited elsewhere";
    let mut out = data[..2].to_vec();
    out.extend_from_slice(&[0xFF, 0xFE]);
    out.extend_from_slice(&((text.len() + 2) as u16).to_be_bytes());
    out.extend_from_slice(text);
    out.extend_from_slice(&data[2..]);
    out
}

async fn call(app: &axum::Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let req = Request::builder().method(method).uri(uri).header("content-type", "application/json");
    let resp = app.clone().oneshot(req.body(Body::from(body.map(|b| b.to_string()).unwrap_or_default())).unwrap()).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn ids_of(g: &Value) -> Vec<i64> {
    let mut v: Vec<i64> = g["members"].as_array().unwrap().iter().map(|m| m["id"].as_i64().unwrap()).collect();
    v.sort_unstable();
    v
}

fn sorted(mut v: Vec<i64>) -> Vec<i64> {
    v.sort_unstable();
    v
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn duplicates_report() {
    unsafe { std::env::set_var("HYPERFOCAL_HELPER_EXE", env!("CARGO_BIN_EXE_hyperfocal")) };
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("photos");
    let (trip, backup, other) = (root.join("2024/Trip"), root.join("Backup"), root.join("2024/Other"));
    for d in [&trip, &backup, &other] {
        std::fs::create_dir_all(d).unwrap();
    }
    let a = jpeg::encode(&scene(1500, 1000, 1), 90, false).unwrap();
    std::fs::write(trip.join("A.JPG"), &a).unwrap();
    std::fs::write(trip.join("A_meta.JPG"), with_comment(&a)).unwrap();
    std::fs::write(backup.join("A.JPG"), &a).unwrap();
    let small = jpeg::resize(scene(1500, 1000, 1), 750, 500).unwrap();
    std::fs::write(trip.join("A_small.JPG"), jpeg::encode(&small, 80, false).unwrap()).unwrap();
    std::fs::write(trip.join("B.JPG"), jpeg::encode(&scene(1500, 1000, 2), 90, false).unwrap()).unwrap();
    std::fs::write(other.join("C.JPG"), jpeg::encode(&scene(1500, 1000, 3), 90, false).unwrap()).unwrap();

    let state = hyperfocal::build_state(hyperfocal::config::Config::for_tests(tmp.path())).await.unwrap();
    {
        // Index, hash the originals (integrity monitor), fingerprint the photos.
        let st = state.clone();
        tokio::task::spawn_blocking(move || {
            assert!(hyperfocal::scan::sync_tree(&st).unwrap());
            let folders: Vec<i64> = {
                let c = st.db.get().unwrap();
                let mut q = c.prepare("SELECT id FROM folders").unwrap();
                q.query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect()
            };
            for id in folders {
                hyperfocal::scan::scan_folder(&st, id, true).unwrap();
            }
            hyperfocal::integrity::session(&st, &mut hyperfocal::integrity::Pace::unlimited(), None, true, true).unwrap();
            assert_eq!(hyperfocal::dupes::worker::hash_now(&st).unwrap(), 6);
            assert_eq!(hyperfocal::dupes::worker::hash_now(&st).unwrap(), 0, "nothing left to hash");
        })
        .await
        .unwrap();
    }
    let (id, folder) = {
        let c = state.db.get().unwrap();
        let mut ids = std::collections::HashMap::new();
        let mut q = c.prepare("SELECT f.path || '/' || p.name, p.id FROM photos p JOIN folders f ON f.id = p.folder_id").unwrap();
        for r in q.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))).unwrap() {
            let (k, v) = r.unwrap();
            ids.insert(k, v);
        }
        let mut folders = std::collections::HashMap::new();
        let mut q = c.prepare("SELECT path, id FROM folders").unwrap();
        for r in q.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))).unwrap() {
            let (k, v) = r.unwrap();
            folders.insert(k, v);
        }
        (move |k: &str| ids[k], move |k: &str| folders[k])
    };
    let (a_trip, a_meta, a_backup, a_small, b) = (id("2024/Trip/A.JPG"), id("2024/Trip/A_meta.JPG"), id("Backup/A.JPG"), id("2024/Trip/A_small.JPG"), id("2024/Trip/B.JPG"));
    let (trip_id, backup_id, other_id) = (folder("2024/Trip"), folder("Backup"), folder("2024/Other"));
    let app = hyperfocal::api::router(state.clone());

    // Exact copies: the one with other metadata in it too.
    let (st, r) = call(&app, "GET", "/api/dupes?mode=exact", None).await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["total_groups"], 1, "{r}");
    assert_eq!(ids_of(&r["groups"][0]), sorted(vec![a_trip, a_meta, a_backup]));
    assert_eq!((r["photos"].as_i64(), r["extra"].as_i64()), (Some(3), Some(2)));
    // The keeper first; the others' bytes are what rejecting them would clear away.
    let m = r["groups"][0]["members"].as_array().unwrap();
    assert_eq!(m[0]["id"], r["groups"][0]["keeper"]);
    let others: i64 = m[1..].iter().map(|x| x["bytes"].as_i64().unwrap()).sum();
    assert_eq!(r["groups"][0]["extra_bytes"].as_i64(), Some(others));
    assert!(m.iter().all(|x| x["bytes"].as_i64().unwrap() >= a.len() as i64));
    assert_eq!((r["covered"].as_i64(), r["eligible"].as_i64()), (Some(6), Some(6)));
    assert!(!r["groups"][0]["reason"].as_str().unwrap().is_empty());
    // Within Trip: two of them. Trip against the library: all three, Backup's outside.
    let (_, r) = call(&app, "GET", &format!("/api/dupes?mode=exact&scope=folder&folder={trip_id}"), None).await;
    assert_eq!(r["total_groups"], 1, "{r}");
    assert_eq!(ids_of(&r["groups"][0]), sorted(vec![a_trip, a_meta]));
    let (_, r) = call(&app, "GET", &format!("/api/dupes?mode=exact&scope=against&folder={backup_id}"), None).await;
    assert_eq!(ids_of(&r["groups"][0]), sorted(vec![a_trip, a_meta, a_backup]));
    let inside: Vec<i64> = r["groups"][0]["members"].as_array().unwrap().iter().filter(|m| m["inside"] == true).map(|m| m["id"].as_i64().unwrap()).collect();
    assert_eq!(inside, vec![a_backup]);
    let (_, r) = call(&app, "GET", &format!("/api/dupes?mode=exact&scope=against&folder={other_id}"), None).await;
    assert_eq!(r["total_groups"], 0, "{r}");

    // Near-duplicates: the smaller copy as well, never the keeper.
    let (_, r) = call(&app, "GET", "/api/dupes?mode=near", None).await;
    assert_eq!(r["total_groups"], 1, "{r}");
    let g = &r["groups"][0];
    assert_eq!(ids_of(g), sorted(vec![a_trip, a_meta, a_backup, a_small]));
    assert_ne!(g["keeper"].as_i64(), Some(a_small));
    let d = g["members"].as_array().unwrap().iter().find(|m| m["id"].as_i64() == Some(a_small)).unwrap()["d"].as_i64().unwrap();
    assert!(d <= 4, "the resized copy is {d} bits away");
    assert_eq!((r["covered"].as_i64(), r["eligible"].as_i64()), (Some(6), Some(6)));

    // Series: A and its small copy a second apart from one camera; B right after is
    // another scene.
    {
        let c = state.db.get().unwrap();
        for (photo, t) in [(a_trip, 1_700_000_000i64), (a_small, 1_700_000_001), (b, 1_700_000_002)] {
            c.execute("UPDATE photos SET taken = ?2, make = 'Canon', model = 'EOS R5' WHERE id = ?1", [photo, t]).unwrap();
        }
    }
    let (_, r) = call(&app, "GET", "/api/dupes?mode=series", None).await;
    assert_eq!(r["total_groups"], 1, "{r}");
    assert_eq!(ids_of(&r["groups"][0]), sorted(vec![a_trip, a_small]));
    let (_, r) = call(&app, "GET", "/api/dupes?mode=series&min=3", None).await;
    assert_eq!(r["total_groups"], 0, "{r}");

    // Not duplicates: gone from both reports, until taken back.
    let all = vec![a_trip, a_meta, a_backup, a_small];
    let (st, r) = call(&app, "POST", "/api/dupes/dismiss", Some(json!({ "ids": all }))).await;
    assert_eq!((st, r["pairs"].as_i64()), (StatusCode::OK, Some(6)));
    let (_, r) = call(&app, "GET", "/api/dupes?mode=near", None).await;
    assert_eq!((r["total_groups"].as_i64(), r["dismissed"].as_i64()), (Some(0), Some(6)), "{r}");
    let (_, r) = call(&app, "GET", "/api/dupes?mode=exact", None).await;
    assert_eq!(r["total_groups"], 0, "{r}");
    let (_, r) = call(&app, "POST", "/api/dupes/dismiss", Some(json!({ "ids": all, "undo": true }))).await;
    assert_eq!(r["pairs"], 6);
    let (_, r) = call(&app, "GET", "/api/dupes?mode=near", None).await;
    assert_eq!(r["total_groups"], 1, "{r}");
    call(&app, "POST", "/api/dupes/dismiss", Some(json!({ "ids": [a_trip, a_small] }))).await;
    let (_, r) = call(&app, "DELETE", "/api/dupes/dismissed", None).await;
    assert_eq!(r["cleared"], 1);

    // The grid shows a group by its ids.
    let (_, r) = call(&app, "GET", &format!("/api/photos?ids={a_trip},{a_backup}"), None).await;
    assert_eq!(sorted(r["id"].as_array().unwrap().iter().map(|v| v.as_i64().unwrap()).collect()), sorted(vec![a_trip, a_backup]));
    let (_, r) = call(&app, "GET", "/api/photos?ids=&count=1", None).await;
    assert_eq!(r["count"], 0);

    // Bad requests, and the worker's state (not running in tests).
    assert_eq!(call(&app, "GET", "/api/dupes?mode=fuzzy", None).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(call(&app, "GET", "/api/dupes?scope=folder", None).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(call(&app, "POST", "/api/dupes/dismiss", Some(json!({ "ids": [a_trip] }))).await.0, StatusCode::BAD_REQUEST);
    let (_, s) = call(&app, "GET", "/api/dupes/status", None).await;
    assert_eq!((s["enabled"].as_bool(), s["phase"].as_str()), (Some(false), Some("off")));
}
