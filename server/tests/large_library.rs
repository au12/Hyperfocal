//! A generated 300,000-photo library, to catch slow queries and memory growth before
//! they reach a real archive.
//!
//! The index is filled with synthetic photos (folders, pairs, ratings, labels, keywords,
//! GPS, cameras, capture times spread over 19 years); no image files are written. Every
//! API call the UI makes against a large library then runs through the real router and
//! is timed. Limits are generous (CI runners are slow and noisy): they catch a query
//! that falls back to a full scan or a sort of the whole table, not a 10% regression.
//!
//! ```text
//! cargo test --release --test large_library -- --ignored --nocapture
//! HYPERFOCAL_LARGE_PHOTOS=50000 cargo test ...   # a smaller library
//! ```

use axum::body::Body;
use axum::http::{Request, StatusCode};
use hyperfocal::config::Config;
use hyperfocal::model::PhotoRec;
use hyperfocal::repo;
use hyperfocal::state::AppState;
use rusqlite::params;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tower::ServiceExt;

/// Deterministic pseudo-random numbers (xorshift64*).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
    fn chance(&mut self, p: f64) -> bool {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64 <= p
    }
    fn pick<'a, T>(&mut self, v: &'a [T]) -> &'a T {
        &v[self.below(v.len() as u64) as usize]
    }
}

const CAMERAS: &[(&str, &str)] = &[
    ("Canon", "Canon EOS R6"),
    ("Canon", "Canon EOS 5D Mark III"),
    ("Canon", "Canon EOS 7D"),
    ("Canon", "Canon EOS R5"),
    ("NIKON CORPORATION", "NIKON D750"),
    ("NIKON CORPORATION", "NIKON Z 6_2"),
    ("SONY", "ILCE-7M3"),
    ("SONY", "ILCE-7RM4"),
    ("FUJIFILM", "X-T3"),
    ("Apple", "iPhone 12 Pro"),
    ("Apple", "iPhone 15 Pro"),
    ("samsung", "SM-S918B"),
    ("Panasonic", "DC-GH5"),
    ("OLYMPUS CORPORATION", "E-M1MarkII"),
    ("Ricoh", "GR III"),
];

const LENSES: &[&str] = &[
    "RF24-70mm F2.8 L IS USM", "RF70-200mm F2.8 L IS USM", "EF50mm f/1.8 STM", "EF24-105mm f/4L IS USM",
    "RF35mm F1.8 MACRO IS STM", "EF100mm f/2.8L Macro IS USM", "AF-S NIKKOR 24-70mm f/2.8G ED", "NIKKOR Z 50mm f/1.8 S",
    "FE 24-70mm F2.8 GM", "FE 85mm F1.8", "XF16-55mmF2.8 R LM WR", "iPhone 12 Pro back triple camera 4.2mm f/1.6",
    "iPhone 15 Pro back triple camera 6.86mm f/1.78", "LEICA DG 12-60/F2.8-4.0", "M.12-40mm F2.8", "GR LENS 18.3mm F2.8",
];

const PLACES: &[(&str, &str, &str, f64, f64)] = &[
    ("Lisbon", "Lisboa", "Portugal", 38.7223, -9.1393),
    ("Asheville", "North Carolina", "United States", 35.5951, -82.5515),
    ("Greenville", "South Carolina", "United States", 34.8526, -82.3940),
    ("Tallahassee", "Florida", "United States", 30.4383, -84.2807),
    ("Kyoto", "Kyoto", "Japan", 35.0116, 135.7681),
    ("Reykjavik", "Capital Region", "Iceland", 64.1466, -21.9426),
    ("Cusco", "Cusco", "Peru", -13.5320, -71.9675),
    ("Edinburgh", "Scotland", "United Kingdom", 55.9533, -3.1883),
    ("Charleston", "South Carolina", "United States", 32.7765, -79.9311),
    ("Banff", "Alberta", "Canada", 51.1784, -115.5708),
];

const EVENTS: &[&str] = &[
    "Birthday", "Beach week", "Hiking", "Wedding", "Christmas", "Thanksgiving", "Road trip", "Concert", "Graduation",
    "Football game", "Garden", "Snow day", "Fall colors", "Zoo", "Aquarium", "Camping", "Fireworks", "Family visit",
    "Portraits", "Street", "Sunset", "Museum", "Kayaking", "Fishing", "Lake house", "Easter", "Halloween", "Baseball",
];

fn keyword_vocabulary() -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for p in PLACES {
        v.push(format!("Places|{}|{}", p.2, p.0));
    }
    for p in ["Ann", "Ben", "Chris", "Dana", "Eli", "Fran", "Gus", "Hana", "Ivan", "Jules", "Kai", "Lena"] {
        v.push(format!("People|Family|{p}"));
    }
    for i in 0..300 {
        v.push(format!("People|Friends|Friend {i:03}"));
    }
    for t in ["dog", "cat", "beach", "mountains", "snow", "sunset", "city", "night", "food", "flowers", "car", "bird", "boat", "bridge"] {
        v.push(t.to_string());
    }
    for i in 0..1500 {
        v.push(format!("tag{i:04}"));
    }
    v
}

fn photo_count() -> usize {
    std::env::var("HYPERFOCAL_LARGE_PHOTOS").ok().and_then(|v| v.parse().ok()).unwrap_or(300_000)
}

fn config(root: &Path) -> Config {
    Config {
        photos_root: root.join("photos"),
        config_dir: root.join("config"),
        cache_dir: root.join("config").join("cache"),
        web_dir: root.join("web"),
        port: 0,
        readonly: true,
        reject_dir: "_Rejected".into(),
        rescan_minutes: 0,
        pregen_thumbs: false,
        raw_cache_mb: 64,
        preview_cache_mb: 64,
        index_threads: 2,
        decode_concurrency: 1,
        password: None,
        exiftool: "exiftool".into(),
        rename_sidecars: false,
        raw_decoder_libraw: false,
        ..Config::for_tests(root)
    }
}

/// Seconds since the epoch for a civil date (camera clock as UTC, like the index).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = if y2 >= 0 { y2 } else { y2 - 399 } / 400;
    let yoe = y2 - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

struct Generated {
    root_id: i64,
    year_ids: Vec<i64>,
    event_ids: Vec<i64>,
    big_event: i64,
    photos: usize,
}

/// Fills the index with `n` visible photos (plus the JPEG halves of RAW+JPEG pairs).
fn generate(state: &AppState, n: usize) -> Generated {
    let mut rng = Rng(0x5eed_1234_abcd_ef01);
    let vocab = keyword_vocabulary();
    let mut c = state.db.get().unwrap();
    let tx = c.transaction().unwrap();
    let folder = |parent: Option<i64>, path: &str, name: &str, reject: bool| -> i64 {
        tx.query_row(
            "INSERT INTO folders(parent_id, path, name, is_reject, seen, scanned_at) VALUES(?1, ?2, ?3, ?4, 1, 1) RETURNING id",
            params![parent, path, name, reject as i64],
            |r| r.get(0),
        )
        .unwrap()
    };
    let root_id = folder(None, "", "Library", false);
    let years: Vec<i64> = (2008..=2026).collect();
    let mut year_ids = Vec::new();
    let mut event_ids = Vec::new();
    // Folders: ~1,500 events across the years, sizes from a handful to a few thousand.
    let mut plan: Vec<(i64, String, i64, usize)> = Vec::new(); // (folder id, path, start day, photos)
    let mut remaining = n;
    let per_year = n / years.len();
    for (yi, &y) in years.iter().enumerate() {
        let yid = folder(Some(root_id), &y.to_string(), &y.to_string(), false);
        year_ids.push(yid);
        let mut left = if yi + 1 == years.len() { remaining } else { per_year.min(remaining) };
        let mut e = 0;
        while left > 0 {
            let size = (match rng.below(20) {
                0 => 800 + rng.below(1600),
                1..=5 => 150 + rng.below(300),
                _ => 10 + rng.below(140),
            } as usize)
                .min(left);
            let month = 1 + rng.below(12) as i64;
            let day = 1 + rng.below(28) as i64;
            let name = format!("{y}-{month:02}-{day:02} {} {e}", rng.pick(EVENTS));
            let path = format!("{y}/{name}");
            let id = folder(Some(yid), &path, &name, false);
            event_ids.push(id);
            plan.push((id, path, days_from_civil(y, month, day), size));
            left -= size;
            remaining -= size;
            e += 1;
        }
    }
    let big_event = plan.iter().max_by_key(|p| p.3).unwrap().0;
    let mut photos = 0usize;
    let mut serial = 0u64;
    for (fid, path, start_day, size) in &plan {
        // About one event in ten has rejects.
        let reject_id = rng.chance(0.1).then(|| folder(Some(*fid), &format!("{path}/_Rejected"), "_Rejected", true));
        let place = rng.chance(0.4).then(|| *rng.pick(PLACES));
        let cam = *rng.pick(CAMERAS);
        let mut t = start_day * 86400 + 8 * 3600 + rng.below(4 * 3600) as i64;
        for _ in 0..*size {
            serial += 1;
            t += 1 + rng.below(90) as i64;
            let is_phone = cam.0 == "Apple" || cam.0 == "samsung";
            let (ext, kind) = if is_phone {
                if rng.chance(0.7) { ("HEIC", 5) } else { ("JPG", 2) }
            } else {
                match rng.below(100) {
                    0..=74 => (if cam.0 == "Canon" { "CR3" } else if cam.0.starts_with("NIKON") { "NEF" } else { "ARW" }, 1),
                    75..=94 => ("JPG", 2),
                    95..=97 => ("DNG", 1),
                    _ => ("TIF", 3),
                }
            };
            let stem = format!("IMG_{:05}", serial % 100_000);
            let rating = match rng.below(100) {
                0..=59 => 0,
                60..=74 => 1,
                75..=84 => 2,
                85..=92 => 3,
                93..=97 => 4,
                _ => 5,
            };
            let mut kw: Vec<String> = Vec::new();
            let mut hk: Vec<String> = Vec::new();
            for _ in 0..rng.below(6) {
                let k = rng.pick(&vocab).clone();
                if k.contains('|') {
                    let leaf = k.rsplit('|').next().unwrap().to_string();
                    if !kw.contains(&leaf) {
                        kw.push(leaf);
                    }
                    if !hk.contains(&k) {
                        hk.push(k);
                    }
                } else if !kw.contains(&k) {
                    kw.push(k);
                }
            }
            let gps = place.filter(|_| rng.chance(0.8)).map(|p| (p.3 + (rng.below(2000) as f64 - 1000.0) * 1e-5, p.4 + (rng.below(2000) as f64 - 1000.0) * 1e-5));
            let in_reject = reject_id.is_some() && rng.chance(0.08);
            let rec = PhotoRec {
                folder_id: if in_reject { reject_id.unwrap() } else { *fid },
                name: format!("{stem}.{ext}"),
                stem: stem.clone(),
                ext: ext.to_ascii_lowercase(),
                kind,
                size: 2_000_000 + rng.below(50_000_000) as i64,
                mtime: t * 1000,
                sidecar: (rating > 0 || !kw.is_empty()).then(|| format!("{stem}.xmp")),
                sidecar_mtime: Some(t * 1000),
                width: Some(6000),
                height: Some(4000),
                orientation: if rng.chance(0.15) { 6 } else { 1 },
                taken: Some(t),
                taken_str: Some(format!("{t}")),
                make: Some(cam.0.into()),
                model: Some(cam.1.into()),
                lens: Some(rng.pick(LENSES).to_string()),
                focal: Some(24.0 + rng.below(176) as f64),
                fnum: Some(1.8 + rng.below(100) as f64 / 10.0),
                exposure: Some(1.0 / (30 + rng.below(4000)) as f64),
                iso: Some(100 << rng.below(7)),
                rating: if rng.chance(0.02) { -1 } else { rating },
                label: rng.chance(0.1).then(|| rng.pick(&["Red", "Yellow", "Green", "Blue", "Purple"]).to_string()),
                pick: if rng.chance(0.05) { 1 } else { 0 },
                title: rng.chance(0.05).then(|| format!("{} with friends", rng.pick(EVENTS))),
                description: rng.chance(0.03).then(|| "A long caption about the day, the light and who was there.".to_string()),
                keywords: kw,
                hkeywords: hk,
                lat: gps.map(|g| g.0),
                lon: gps.map(|g| g.1),
                city: gps.and(place.map(|p| p.0.to_string())),
                state: gps.and(place.map(|p| p.1.to_string())),
                country: gps.and(place.map(|p| p.2.to_string())),
                ..Default::default()
            };
            let folder_path = if in_reject { format!("{path}/_Rejected") } else { path.clone() };
            let id = repo::upsert(&tx, &rec).unwrap();
            repo::index_text(&tx, id, &rec, &folder_path).unwrap();
            photos += 1;
            // RAW+JPEG pairs: the JPEG is its own row, folded into the RAW.
            if kind == 1 && rng.chance(0.15) {
                let jpg = PhotoRec { name: format!("{stem}.JPG"), ext: "jpg".into(), kind: 2, size: rec.size / 4, ..rec.clone() };
                let jid = repo::upsert(&tx, &jpg).unwrap();
                repo::index_text(&tx, jid, &jpg, &folder_path).unwrap();
            }
        }
        repo::refresh_folder(&tx, *fid).unwrap();
        if let Some(r) = reject_id {
            repo::refresh_folder(&tx, r).unwrap();
        }
    }
    tx.commit().unwrap();
    Generated { root_id, year_ids, event_ids, big_event, photos }
}

/// People and pets: faces in about half the photos (1 to 4 each), a family of 12 seen
/// everywhere, 300 friends, 2,000 unnamed groups and unknown faces; dogs and cats in a
/// few. Returns (a family member, a pet, an unnamed group, a photo with faces).
fn generate_people(state: &AppState) -> (i64, i64, i64, i64, usize) {
    let mut rng = Rng(0xface_5eed_0042_1234);
    let mut c = state.db.get().unwrap();
    let tx = c.transaction().unwrap();
    let photos: Vec<i64> = {
        let mut st = tx.prepare("SELECT id FROM photos WHERE pair_of IS NULL ORDER BY id").unwrap();
        st.query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect()
    };
    let subject = |kind: i64, species: i64, name: Option<String>| -> i64 {
        tx.query_row(
            "INSERT INTO subjects(kind, species, name, hidden, created, updated) VALUES(?1, ?2, ?3, 0, 0, 0) RETURNING id",
            params![kind, species, name],
            |r| r.get(0),
        )
        .unwrap()
    };
    let family: Vec<i64> = ["Ann", "Ben", "Chris", "Dana", "Eli", "Fran", "Gus", "Hana", "Ivan", "Jules", "Kai", "Lena"]
        .iter()
        .map(|n| subject(1, 1, Some(n.to_string())))
        .collect();
    let friends: Vec<i64> = (0..300).map(|i| subject(1, 1, Some(format!("Friend {i:03}")))).collect();
    let groups: Vec<i64> = (0..2000).map(|_| subject(1, 1, None)).collect();
    let pets = [subject(2, 2, Some("Rex".into())), subject(2, 3, Some("Mittens".into())), subject(2, 2, Some("Biscuit".into()))];
    let mut ins = tx
        .prepare(
            "INSERT INTO detections(photo_id, kind, x, y, w, h, score, quality, emb, subject_id, state, sim, source, created)
             VALUES(?1, ?2, ?3, 0.3, 0.08, 0.1, 0.9, ?4, NULL, ?5, ?6, ?7, 0, 0)",
        )
        .unwrap();
    let mut n = 0usize;
    let mut with_faces = 0i64;
    for &p in &photos {
        if rng.chance(0.5) {
            let faces = 1 + rng.below(4);
            let mut seen = std::collections::HashSet::new();
            for f in 0..faces {
                let who: Option<i64> = match rng.below(100) {
                    0..=49 => Some(*rng.pick(&family)),
                    50..=64 => Some(*rng.pick(&friends)),
                    65..=74 => Some(*rng.pick(&groups)),
                    _ => None,
                };
                let who = who.filter(|w| seen.insert(*w));
                let named = who.is_some_and(|w| !groups.contains(&w));
                let state = if named && rng.chance(0.7) { 1 } else { 0 };
                ins.execute(params![p, 1, 0.1 + f as f64 * 0.2, rng.below(100) as f64 / 100.0, who, state, who.map(|_| 0.5)]).unwrap();
                n += 1;
            }
            with_faces = p;
        }
        if rng.chance(0.08) {
            let pet = *rng.pick(&pets);
            ins.execute(params![p, if pet == pets[1] { 3 } else { 2 }, 0.5, 0.9, Some(pet), 1, Option::<f64>::None]).unwrap();
            n += 1;
        }
        // Analysed (so the status counts and the photo panels see them).
        tx.execute("INSERT INTO analysis(photo_id, size, mtime, orientation, models, at, error, w, h) SELECT id, size, mtime, orientation, 'm', 0, NULL, 2048, 1365 FROM photos WHERE id = ?1", [p]).unwrap();
    }
    drop(ins);
    tx.commit().unwrap();
    (family[0], pets[0], groups[0], with_faces, n)
}

fn rss_mb() -> (f64, f64) {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let field = |k: &str| -> f64 {
        s.lines()
            .find(|l| l.starts_with(k))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.0)
            / 1024.0
    };
    (field("VmRSS:"), field("VmHWM:"))
}

struct Bench {
    app: axum::Router,
    rows: BTreeMap<String, (Duration, usize, Duration)>,
}

impl Bench {
    /// GET `uri` three times (cold, then warm) and keep the median.
    async fn get(&mut self, label: &str, uri: &str, limit: Duration) -> Value {
        let mut times = Vec::new();
        let mut body = Value::Null;
        let mut size = 0;
        for _ in 0..3 {
            let t = Instant::now();
            let resp = self.app.clone().oneshot(Request::get(uri).body(Body::empty()).unwrap()).await.unwrap();
            let status = resp.status();
            let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
            times.push(t.elapsed());
            assert_eq!(status, StatusCode::OK, "{uri}: {}", String::from_utf8_lossy(&bytes));
            size = bytes.len();
            body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        }
        times.sort();
        let median = times[1];
        self.rows.insert(label.to_string(), (median, size, limit));
        body
    }

    fn report(&self) -> Vec<String> {
        println!("\n{:<42} {:>10} {:>10} {:>10}", "query", "median", "limit", "bytes");
        let mut slow = Vec::new();
        for (k, (t, n, limit)) in &self.rows {
            let flag = if t > limit { "  << SLOW" } else { "" };
            println!("{k:<42} {:>8.1}ms {:>8.0}ms {n:>10}{flag}", t.as_secs_f64() * 1000.0, limit.as_secs_f64() * 1000.0);
            if t > limit {
                slow.push(format!("{k}: {:.0} ms (limit {:.0} ms)", t.as_secs_f64() * 1000.0, limit.as_secs_f64() * 1000.0));
            }
        }
        slow
    }
}

fn ms(v: u64) -> Duration {
    Duration::from_millis(v)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "generates a 300,000-photo index; run with --ignored in release mode"]
async fn large_library() {
    // Never re-run this test binary as the native helper.
    // SAFETY: set before any thread of this test reads the environment.
    unsafe { std::env::set_var("HYPERFOCAL_HELPER_EXE", env!("CARGO_BIN_EXE_hyperfocal")) };
    let n = photo_count();
    let tmp = tempfile::tempdir().unwrap();
    // HYPERFOCAL_LARGE_KEEP=DIR keeps the generated index there, for EXPLAIN QUERY PLAN.
    let dir = std::env::var_os("HYPERFOCAL_LARGE_KEEP").map(std::path::PathBuf::from).unwrap_or_else(|| tmp.path().to_path_buf());
    let _ = std::fs::remove_dir_all(dir.join("config"));
    std::fs::create_dir_all(dir.join("photos")).unwrap();
    let state: Arc<AppState> = hyperfocal::build_state(config(&dir)).await.unwrap();
    let t = Instant::now();
    let g = {
        let st = state.clone();
        tokio::task::spawn_blocking(move || generate(&st, n)).await.unwrap()
    };
    let (ann, rex, group, face_photo, detections) = {
        let st = state.clone();
        tokio::task::spawn_blocking(move || generate_people(&st)).await.unwrap()
    };
    let gen_time = t.elapsed();
    hyperfocal::db::optimize(&state.db.get().unwrap()).unwrap();
    println!("generated {detections} faces and pets");
    let db_mb = std::fs::metadata(dir.join("config/hyperfocal.db")).map(|m| m.len()).unwrap_or(0) as f64 / 1048576.0;
    println!(
        "generated {} photos in {} folders in {:.1}s; database {db_mb:.0} MB; RSS {:.0} MB",
        g.photos,
        g.event_ids.len() + g.year_ids.len() + 1,
        gen_time.as_secs_f64(),
        rss_mb().0
    );
    assert_eq!(g.photos, n);

    let mut b = Bench { app: hyperfocal::api::router(state.clone()), rows: BTreeMap::new() };
    let root = g.root_id;
    let year = g.year_ids[g.year_ids.len() / 2];
    let event = g.event_ids[g.event_ids.len() / 3];
    let big = g.big_event;
    let mut rss_after: Vec<f64> = Vec::new();
    // Rounds of the same queries until memory settles (each pooled connection warms its
    // own page cache, and the allocator keeps what it freed, so the first rounds grow).
    for round in 0..6 {
        b.get("status", "/api/status", ms(100)).await;
        let tree = b.get("tree", "/api/tree", ms(200)).await;
        if round == 0 {
            assert!(tree["folders"].as_array().unwrap().len() > 1000);
        }
        let all = b.get("photos: whole library (100k)", &format!("/api/photos?folder={root}&rec=1"), ms(800)).await;
        if round == 0 {
            assert_eq!(all["id"].as_array().unwrap().len(), 100_000.min(n));
        }
        b.get("photos: whole library, all", &format!("/api/photos?folder={root}&rec=1&limit=250000"), ms(2000)).await;
        b.get("photos: year", &format!("/api/photos?folder={year}&rec=1"), ms(300)).await;
        b.get("photos: event folder", &format!("/api/photos?folder={event}"), ms(50)).await;
        b.get("photos: biggest event", &format!("/api/photos?folder={big}"), ms(150)).await;
        b.get("photos: library 4+ stars", &format!("/api/photos?folder={root}&rec=1&rmin=4"), ms(500)).await;
        b.get("photos: library picks", &format!("/api/photos?folder={root}&rec=1&pick=1"), ms(500)).await;
        b.get("photos: library red label", &format!("/api/photos?folder={root}&rec=1&lc=red"), ms(300)).await;
        b.get("photos: library without GPS", &format!("/api/photos?folder={root}&rec=1&gps=0"), ms(800)).await;
        b.get("photos: library one camera", &format!("/api/photos?folder={root}&rec=1&cam=Canon%20EOS%207D"), ms(300)).await;
        b.get("photos: library keyword", &format!("/api/photos?folder={root}&rec=1&kw=sunset"), ms(200)).await;
        b.get("photos: library by name", &format!("/api/photos?folder={root}&rec=1&sort=name"), ms(1000)).await;
        b.get("photos: library by rating", &format!("/api/photos?folder={root}&rec=1&sort=rating&desc=1"), ms(2000)).await;
        b.get("photos: rejected only", &format!("/api/photos?folder={root}&rec=1&rej=2"), ms(200)).await;
        b.get("photos: date range (one year)", &format!("/api/photos?folder={root}&rec=1&from={}&to={}", days_from_civil(2015, 1, 1) * 86400, days_from_civil(2016, 1, 1) * 86400), ms(300)).await;
        b.get("search: word", &format!("/api/photos?folder={root}&rec=1&q=lisbon"), ms(300)).await;
        b.get("search: two words", &format!("/api/photos?folder={root}&rec=1&q=beach%20canon"), ms(300)).await;
        b.get("search: file name", &format!("/api/photos?folder={root}&rec=1&q=IMG_04217"), ms(150)).await;
        let sub = b.get("search: substring", &format!("/api/photos?folder={root}&rec=1&q=ortug"), ms(300)).await;
        if round == 0 {
            // "ortug" is inside "Portugal" (keywords, country): substring search finds it.
            assert!(sub["id"].as_array().unwrap().len() > 1000, "substring search found too little");
        }
        b.get("facets", "/api/facets", ms(700)).await;
        let tl = b.get("timeline", "/api/timeline", ms(400)).await;
        if round == 0 {
            let days: i64 = tl["days"].as_array().unwrap().iter().map(|d| d[1].as_i64().unwrap()).sum();
            assert!(days > (n as i64) * 9 / 10, "timeline covers {days} photos");
        }
        b.get("count: library 5 stars", &format!("/api/photos?folder={root}&rec=1&rmin=5&count=1"), ms(300)).await;
        b.get("count: picks in a year", &format!("/api/photos?folder={root}&rec=1&pick=1&count=1&from={}&to={}", days_from_civil(2019, 1, 1) * 86400, days_from_civil(2020, 1, 1) * 86400), ms(300)).await;
        b.get("collections", "/api/collections", ms(50)).await;
        b.get("keywords: suggest", "/api/keywords?q=fri", ms(400)).await;
        b.get("map: world clusters", &format!("/api/geo/clusters?folder={root}&rec=1&zoom=2"), ms(500)).await;
        b.get("map: city clusters", &format!("/api/geo/clusters?folder={root}&rec=1&zoom=12&west=-9.3&east=-9.0&south=38.6&north=38.8"), ms(200)).await;
        let detail = b.get("photo detail", &format!("/api/photos?folder={event}&limit=1"), ms(100)).await;
        if let Some(id) = detail["id"].as_array().and_then(|a| a.first()).and_then(Value::as_i64) {
            b.get("photo: detail", &format!("/api/photo/{id}"), ms(100)).await;
        }
        // People & pets.
        b.get("people: status", "/api/people/status", ms(800)).await;
        let named = b.get("people: named", "/api/people?kind=person&named=named&limit=2000", ms(1500)).await;
        if round == 0 {
            assert!(named["subjects"].as_array().unwrap().len() >= 300, "{}", named["total"]);
        }
        b.get("people: unnamed groups", "/api/people?kind=person&named=unnamed&min=2&limit=600", ms(1500)).await;
        b.get("people: pets", "/api/people?kind=pet&named=named", ms(400)).await;
        b.get("people: one person", &format!("/api/people/{ann}"), ms(200)).await;
        b.get("people: to review", &format!("/api/people/{ann}/detections?state=auto&limit=240"), ms(300)).await;
        b.get("people: confirmed", &format!("/api/people/{ann}/detections?state=confirmed&limit=240"), ms(300)).await;
        b.get("people: a group", &format!("/api/people/{group}/detections?limit=240"), ms(100)).await;
        b.get("people: names", "/api/people/names?q=fr", ms(300)).await;
        b.get("people: photo", &format!("/api/photo/{face_photo}/people"), ms(50)).await;
        let pa = b.get("photos: one person", &format!("/api/photos?folder={root}&rec=1&subj={ann}"), ms(600)).await;
        if round == 0 {
            assert!(pa["id"].as_array().unwrap().len() > 1000);
        }
        b.get("photos: person and pet", &format!("/api/photos?folder={root}&rec=1&subj={ann},{rex}"), ms(600)).await;
        rss_after.push(rss_mb().0);
        if round >= 2 && rss_after[round] - rss_after[round - 1] < 64.0 {
            break;
        }
    }
    let slow = b.report();
    let (rss, hwm) = rss_mb();
    println!("\nRSS {rss:.0} MB (peak {hwm:.0} MB); after each round: {rss_after:.0?}");
    // Memory must settle: a round of identical queries may not keep growing.
    let k = rss_after.len();
    let growth = rss_after[k - 1] - rss_after[k - 2];
    assert!(growth < 64.0, "memory grew by {growth:.0} MB between identical rounds: {rss_after:?}");
    assert!(slow.is_empty(), "slow queries:\n{}", slow.join("\n"));
}
