//! Thumbnails and previews through the native helper (the built binary): HEIC drafts
//! from the embedded thumbnail, and previews of recent imports made ahead at idle.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use hyperfocal::images::{self, jpeg};
use std::path::Path;
use tower::ServiceExt;

fn files_under(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p.file_name().unwrap().to_string_lossy().into_owned());
            }
        }
    }
    out.sort();
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn heic_drafts_and_previews_ahead() {
    // The helper is this build's binary (a test binary cannot be it).
    unsafe { std::env::set_var("HYPERFOCAL_HELPER_EXE", env!("CARGO_BIN_EXE_hyperfocal")) };
    if !hyperfocal::native::HEIC {
        eprintln!("skipping: no HEIC support in this build");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("photos");
    std::fs::create_dir_all(lib.join("2024/Phone/_Rejected")).unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/thumbnail.heic");
    std::fs::copy(&fixture, lib.join("2024/Phone/IMG_0001.HEIC")).unwrap();
    std::fs::copy(&fixture, lib.join("2024/Phone/_Rejected/IMG_0002.HEIC")).unwrap();
    let img = jpeg::Rgb { w: 1800, h: 1200, px: (0..1800 * 1200 * 3).map(|i| (i % 251) as u8).collect() };
    std::fs::write(lib.join("2024/Phone/IMG_0003.JPG"), jpeg::encode(&img, 85, false).unwrap()).unwrap();
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
    let heic: i64 = state.db.get().unwrap().query_row("SELECT id FROM photos WHERE name = 'IMG_0001.HEIC'", [], |r| r.get(0)).unwrap();
    let app = hyperfocal::api::router(state.clone());
    let get = |uri: String| {
        let app = app.clone();
        async move {
            let resp = app.oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap()).await.unwrap();
            let cc = resp.headers().get("cache-control").map(|v| v.to_str().unwrap().to_string()).unwrap_or_default();
            (resp.status(), cc)
        }
    };
    let (st, cc) = get(format!("/api/photo/{heic}/thumb?v=1")).await;
    assert_eq!((st, cc.as_str()), (StatusCode::OK, "no-store"), "the embedded 320 px thumbnail is a draft");
    assert_eq!(get(format!("/api/photo/{heic}/thumb?v=1")).await.1, "no-store");
    let rec = hyperfocal::repo::load(&state.db.get().unwrap(), heic).unwrap().unwrap().rec;
    let sig = images::sig48(&rec);
    use images::thumbstore::Kind as TK;
    assert!(state.images.thumbs.contains(heic, sig, TK::Draft) && !state.images.thumbs.contains(heic, sig, TK::Thumb));
    // The queue's turn: the full thumbnail replaces the draft.
    assert!(images::build_thumb(&state, heic));
    let (_, cc) = get(format!("/api/photo/{heic}/thumb?v=1")).await;
    assert!(cc.contains("immutable"), "{cc}");
    assert!(!state.images.thumbs.contains(heic, sig, TK::Draft) && state.images.thumbs.contains(heic, sig, TK::Thumb));
    // Thumbnails live in pack files, not one file per photo.
    let files = files_under(&tmp.path().join("config/cache/thumbs"));
    assert!(files.iter().all(|f| f.ends_with(".pack") || f.ends_with(".hint")), "{files:?}");

    // Previews ahead: recent imports outside reject folders, once.
    let since = hyperfocal::db::now_ms() - 3 * 3_600_000;
    let pass = |since: i64| {
        let st = state.clone();
        async move { tokio::task::spawn_blocking(move || images::pregen_pass(&st, since, &|| false)).await.unwrap() }
    };
    assert_eq!(pass(since).await, 2, "the HEIC and the JPEG; not the rejected one");
    let previews = files_under(&tmp.path().join("config/cache/previews"));
    assert_eq!(previews.len(), 2, "{previews:?}");
    assert!(previews.iter().all(|p| p.ends_with("_2048.jpg")), "at the size viewers ask for most: {previews:?}");
    assert_eq!(pass(since).await, 0, "made once");
    // Indexed before pre-rendering started: the library as it was, left alone.
    assert_eq!(pass(hyperfocal::db::now_ms()).await, 0);
}
