//! What a RAW development the viewer gives up on costs the next one. Run with
//!   HYPERFOCAL_BENCH_RAWS=a.CR3,b.CR2 cargo test --release --test cancel_bench -- --ignored --nocapture
//! A request given up on is a future dropped at its `.await`, as hyper drops a handler when
//! the browser cancels the fetch (the single view does for the photos it moves past).

use hyperfocal::images::raw::WbMode;
use hyperfocal::images::tone::ToneMode;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const COPIES: usize = 6;

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn abandoned_developments() {
    let Ok(list) = std::env::var("HYPERFOCAL_BENCH_RAWS") else { return };
    for src in list.split(',').map(PathBuf::from) {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = hyperfocal::config::Config::for_tests(tmp.path());
        cfg.raw_cache_mb = 4096;
        let dir = cfg.photos_root.join("Shoot");
        std::fs::create_dir_all(&dir).unwrap();
        let ext = src.extension().unwrap().to_string_lossy().to_string();
        // Copies, so every development starts cold (nothing cached in memory).
        for i in 0..COPIES * 4 {
            std::fs::copy(&src, dir.join(format!("P{i:02}.{ext}"))).unwrap();
        }
        let state = hyperfocal::build_state(cfg).await.unwrap();
        assert!(hyperfocal::scan::sync_tree(&state).unwrap());
        let folder: i64 = state.db.get().unwrap().query_row("SELECT id FROM folders WHERE path = 'Shoot'", [], |r| r.get(0)).unwrap();
        hyperfocal::scan::scan_folder(&state, folder, true).unwrap();
        let ids: Vec<i64> = {
            let c = state.db.get().unwrap();
            let mut q = c.prepare("SELECT id FROM photos ORDER BY name").unwrap();
            q.query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect()
        };
        let locs: Vec<_> = ids.iter().map(|id| hyperfocal::repo::load(&state.db.get().unwrap(), *id).unwrap().unwrap()).collect();
        let render = |i: usize, ev: f32| state.images.render(&state, &locs[i], ToneMode::Normal, ev, WbMode::Asshot, 2048, true);
        // Warm the page cache.
        for l in &locs {
            let _ = std::fs::read(hyperfocal::scan::photo_path(&state, &l.folder_path, &l.rec.name)).unwrap();
        }

        // A development on its own.
        let mut alone = Vec::new();
        for i in 0..COPIES {
            let t = Instant::now();
            render(i, 0.0).await.unwrap();
            alone.push(t.elapsed().as_secs_f64() * 1e3);
        }
        let full = median(alone);
        let abort = Duration::from_secs_f64(full * 0.3 / 1e3);

        // Moved on: the photo's render is given up 30% in, the next photo's is wanted.
        let mut next = Vec::new();
        for k in 0..COPIES {
            let (a, b) = (COPIES + 2 * k, COPIES + 2 * k + 1);
            let _ = tokio::time::timeout(abort, render(a, 0.0)).await;
            let t = Instant::now();
            render(b, 0.0).await.unwrap();
            next.push(t.elapsed().as_secs_f64() * 1e3);
            // Let anything left of the abandoned one finish before the next round.
            tokio::time::sleep(Duration::from_millis((full * 1.5) as u64)).await;
        }

        // Two requests for one photo (its render and its RAW info); the render is given
        // up 30% in, the RAW info is still wanted.
        let mut shared = Vec::new();
        for k in 0..COPIES {
            let i = COPIES * 3 + k;
            let t = Instant::now();
            let first = tokio::time::timeout(abort, render(i, 0.0));
            let second = async {
                tokio::time::sleep(Duration::from_millis(5)).await;
                state.images.developed(&state, &locs[i], WbMode::Asshot, true).await
            };
            let (_, info) = tokio::join!(first, second);
            info.unwrap();
            shared.push(t.elapsed().as_secs_f64() * 1e3);
            tokio::time::sleep(Duration::from_millis((full * 1.5) as u64)).await;
        }
        println!(
            "{}: develop + render alone {full:.0} ms; next photo after one given up 30% in {:.0} ms; RAW info when the render of the same photo is given up 30% in {:.0} ms",
            src.file_name().unwrap().to_string_lossy(),
            median(next),
            median(shared)
        );
    }
}

/// The culling pass develops a photo in the background when the viewer asks for one.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn background_development_gives_way() {
    use hyperfocal::images::cancel::{self, CancelToken};
    use hyperfocal::images::raw;
    let Ok(list) = std::env::var("HYPERFOCAL_BENCH_RAWS") else { return };
    for src in list.split(',').map(PathBuf::from) {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = hyperfocal::config::Config::for_tests(tmp.path());
        cfg.raw_cache_mb = 4096;
        let dir = cfg.photos_root.join("Shoot");
        std::fs::create_dir_all(&dir).unwrap();
        let ext = src.extension().unwrap().to_string_lossy().to_string();
        // Photos the viewer asks for (alone, then in two rounds), and the culling pass's.
        for i in 0..COPIES * 4 {
            std::fs::copy(&src, dir.join(format!("P{i:02}.{ext}"))).unwrap();
        }
        let state = hyperfocal::build_state(cfg).await.unwrap();
        assert!(hyperfocal::scan::sync_tree(&state).unwrap());
        let folder: i64 = state.db.get().unwrap().query_row("SELECT id FROM folders WHERE path = 'Shoot'", [], |r| r.get(0)).unwrap();
        hyperfocal::scan::scan_folder(&state, folder, true).unwrap();
        let ids: Vec<i64> = {
            let c = state.db.get().unwrap();
            let mut q = c.prepare("SELECT id FROM photos ORDER BY name").unwrap();
            q.query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect()
        };
        let locs: Vec<_> = ids.iter().map(|id| hyperfocal::repo::load(&state.db.get().unwrap(), *id).unwrap().unwrap()).collect();
        let render = |i: usize| state.images.render(&state, &locs[i], ToneMode::Normal, 0.0, WbMode::Asshot, 2048, true);
        for l in &locs {
            let _ = std::fs::read(hyperfocal::scan::photo_path(&state, &l.folder_path, &l.rec.name)).unwrap();
        }
        let mut alone = Vec::new();
        for i in 0..COPIES {
            let t = Instant::now();
            render(i).await.unwrap();
            alone.push(t.elapsed().as_secs_f64() * 1e3);
        }
        let full = median(alone);
        let mut out = Vec::new();
        for (round, preemptible) in [false, true].into_iter().enumerate() {
            let mut lat = Vec::new();
            for k in 0..COPIES {
                let token = if preemptible { state.images.background_token() } else { CancelToken::new() };
                let path = dir.join(format!("P{:02}.{ext}", COPIES * 3 + k));
                let bg = std::thread::spawn(move || cancel::scope(&token, || raw::develop_path(&path, WbMode::Asshot, raw::UE_EV)).is_ok());
                tokio::time::sleep(Duration::from_secs_f64(full * 0.3 / 1e3)).await;
                let t = Instant::now();
                render(COPIES * (round + 1) + k).await.unwrap();
                lat.push(t.elapsed().as_secs_f64() * 1e3);
                bg.join().unwrap();
            }
            out.push(median(lat));
        }
        println!(
            "{}: develop + render alone {full:.0} ms; with a culling development running {:.0} ms, when it gives way {:.0} ms",
            src.file_name().unwrap().to_string_lossy(),
            out[0],
            out[1]
        );
    }
}
