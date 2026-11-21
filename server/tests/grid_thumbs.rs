//! The grid's thumbnail requests: made ones served straight from the cache, and a
//! request the browser gives up on (a tile scrolled past) does not leave the server
//! believing a viewer is still waiting, which would stop the background builder.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use hyperfocal::images::{self, jpeg};
use std::time::Duration;
use tower::ServiceExt;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn grid_thumbnails() {
    unsafe { std::env::set_var("HYPERFOCAL_HELPER_EXE", env!("CARGO_BIN_EXE_hyperfocal")) };
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("photos/2024");
    std::fs::create_dir_all(&lib).unwrap();
    let img = jpeg::Rgb { w: 3000, h: 2000, px: (0..3000 * 2000 * 3).map(|i| (i % 251) as u8).collect() };
    let data = jpeg::encode(&img, 85, false).unwrap();
    for n in ["A.JPG", "B.JPG"] {
        std::fs::write(lib.join(n), &data).unwrap();
    }
    let state = hyperfocal::build_state(hyperfocal::config::Config::for_tests(tmp.path())).await.unwrap();
    {
        let st = state.clone();
        tokio::task::spawn_blocking(move || {
            assert!(hyperfocal::scan::sync_tree(&st).unwrap());
            let ids: Vec<i64> = {
                let c = st.db.get().unwrap();
                let mut q = c.prepare("SELECT id FROM folders").unwrap();
                q.query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect()
            };
            for id in ids {
                hyperfocal::scan::scan_folder(&st, id, true).unwrap();
            }
        })
        .await
        .unwrap();
    }
    let load = |name: &str| {
        let c = state.db.get().unwrap();
        let id: i64 = c.query_row("SELECT id FROM photos WHERE name = ?1", [name], |r| r.get(0)).unwrap();
        hyperfocal::repo::load(&c, id).unwrap().unwrap()
    };
    let (a, b) = (load("A.JPG"), load("B.JPG"));

    // A request dropped while its thumbnail is being made (the browser cancelled it).
    assert!(!state.images.busy());
    let r = tokio::time::timeout(Duration::ZERO, state.images.thumb(&state, &b)).await;
    assert!(r.is_err(), "the thumbnail cannot be ready on the first poll");
    assert!(!state.images.busy(), "a cancelled request must not count as a viewer waiting");

    // Made on first request, then served from the cache by id and version alone.
    let app = hyperfocal::api::router(state.clone());
    let get = |uri: String| {
        let app = app.clone();
        async move {
            let resp = app.oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap()).await.unwrap();
            let cc = resp.headers().get("cache-control").map(|v| v.to_str().unwrap().to_string()).unwrap_or_default();
            let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
            (cc, body.len())
        }
    };
    let v = images::sig_str(&a.rec);
    let (cc, n) = get(format!("/api/photo/{}/thumb?v={v}", a.rec.id)).await;
    assert!(cc.contains("immutable") && n > 1000, "{cc} {n}");
    // The index no longer knows the photo: the cached file still answers its URL.
    state.db.get().unwrap().execute("DELETE FROM photos WHERE id = ?1", [a.rec.id]).unwrap();
    let (cc, n2) = get(format!("/api/photo/{}/thumb?v={v}", a.rec.id)).await;
    assert!(cc.contains("immutable") && n2 == n, "{cc} {n2}");

    // Nothing but a version string names a file.
    let resp = app.clone().oneshot(Request::builder().uri(format!("/api/photo/{}/thumb?v=..%2F..%2Fx", a.rec.id)).body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert!(state.images.cached_thumb(a.rec.id, "abc/../x").is_none());
    assert!(state.images.cached_thumb(-1, &v).is_none());
    assert_eq!(images::parse_sig("1f2e"), Some(0x1f2e));
    for bad in ["ABC", "", "abc/../x", "1234567890abc", "-1"] {
        assert_eq!(images::parse_sig(bad), None, "{bad}");
    }
}
