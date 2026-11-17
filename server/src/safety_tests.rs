//! End-to-end safety test: run the real write paths (ExifTool sidecar writes,
//! reject and restore moves, undo) against a scratch library and prove that no
//! original image file is ever changed, lost, duplicated or overwritten.
//!
//! Needs `exiftool` on PATH (the CI job installs it); without it the test is skipped.

use crate::config::Config;
use crate::meta::xmp::Xmp;
use crate::ops::{self, Patch};
use crate::scan;
use crate::state::AppState;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

fn exiftool_available() -> bool {
    std::process::Command::new("exiftool").arg("-ver").output().map(|o| o.status.success()).unwrap_or(false)
}

fn test_config(root: &Path) -> Config {
    Config {
        photos_root: root.join("photos"),
        config_dir: root.join("config"),
        cache_dir: root.join("config").join("cache"),
        web_dir: root.join("web"),
        port: 0,
        readonly: false,
        reject_dir: "_Rejected".into(),
        rescan_minutes: 0,
        pregen_thumbs: false,
        raw_cache_mb: 16,
        preview_cache_mb: 16,
        index_threads: 2,
        decode_concurrency: 1,
        password: None,
        exiftool: "exiftool".into(),
    }
}

/// Deterministic, file-specific bytes: the contents only need to be unique.
fn write_bytes(path: &Path, len: usize, seed: u32) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut x = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
    let data: Vec<u8> = (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x >> 24) as u8
        })
        .collect();
    std::fs::write(path, data).unwrap();
}

/// Relative path -> SHA-256 of every file below `root`, sidecars excluded.
fn originals(root: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else if !p.extension().is_some_and(|x| x.eq_ignore_ascii_case("xmp")) {
                let hash = format!("{:x}", Sha256::digest(std::fs::read(&p).unwrap()));
                out.insert(p.strip_prefix(root).unwrap().to_string_lossy().into_owned(), hash);
            }
        }
    }
    out
}

/// Every file name below `root` (to catch temp files and stray backups).
fn all_files(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p.strip_prefix(root).unwrap().to_string_lossy().into_owned());
            }
        }
    }
    out.sort();
    out
}

fn scan_all(state: &Arc<AppState>) {
    assert!(scan::sync_tree(state).unwrap(), "library root must be reachable");
    let ids: Vec<i64> = {
        let c = state.db.get().unwrap();
        let mut st = c.prepare("SELECT id FROM folders ORDER BY path").unwrap();
        st.query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect()
    };
    for id in ids {
        scan::scan_folder(state, id, true).unwrap();
    }
}

fn photo_id(state: &AppState, folder: &str, name: &str) -> i64 {
    let c = state.db.get().unwrap();
    c.query_row(
        "SELECT p.id FROM photos p JOIN folders f ON f.id = p.folder_id WHERE f.path = ?1 AND p.name = ?2",
        [folder, name],
        |r| r.get(0),
    )
    .unwrap_or_else(|e| panic!("{folder}/{name}: {e}"))
}

fn sidecar(lib: &Path, rel: &str) -> Xmp {
    Xmp::parse(&std::fs::read(lib.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}")))
}

const EXCIRE_SIDECAR: &str = r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
   xmp:Rating="2"
   xmp:CreatorTool="Excire Foto"
   crs:Exposure2012="+0.50">
   <dc:subject><rdf:Bag><rdf:li>FromExcire</rdf:li></rdf:Bag></dc:subject>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn originals_survive_every_write_path() {
    if !exiftool_available() {
        eprintln!("skipping: exiftool is not installed");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("photos");
    // A RAW+JPEG pair, a RAW with an existing Excire sidecar, a DNG, another year,
    // and a decoy already sitting in _Rejected under the DNG's name.
    write_bytes(&lib.join("2024/Trip/IMG_0001.CR2"), 300_000, 1);
    write_bytes(&lib.join("2024/Trip/IMG_0001.JPG"), 40_000, 2);
    write_bytes(&lib.join("2024/Trip/IMG_0002.CR3"), 250_000, 3);
    std::fs::write(lib.join("2024/Trip/IMG_0002.xmp"), EXCIRE_SIDECAR).unwrap();
    write_bytes(&lib.join("2024/Trip/R0000001.DNG"), 200_000, 4);
    write_bytes(&lib.join("2024/Trip/_Rejected/R0000001.DNG"), 1_000, 5);
    write_bytes(&lib.join("2023/Other/IMG_9999.CR3"), 100_000, 6);
    let before = originals(&lib);
    assert_eq!(before.len(), 6);

    let state = crate::build_state(test_config(tmp.path())).await.unwrap();
    assert!(!state.is_readonly());
    {
        let st = state.clone();
        tokio::task::spawn_blocking(move || scan_all(&st)).await.unwrap();
    }
    let p1 = photo_id(&state, "2024/Trip", "IMG_0001.CR2");
    let p2 = photo_id(&state, "2024/Trip", "IMG_0002.CR3");
    let p3 = photo_id(&state, "2024/Trip", "R0000001.DNG");

    // 1. Bulk metadata edit: new sidecars for p1/p3, a merge into Excire's sidecar for p2.
    let patch: Patch = serde_json::from_value(serde_json::json!({
        "rating": 5, "label": "Red", "pick": 1, "title": "Test title",
        "keywords_add": ["Places|Portugal|Lisbon"],
        "gps": {"lat": 38.7223, "lon": -9.1393, "alt": 12.0},
        "city": "Lisbon", "country": "Portugal"
    }))
    .unwrap();
    let meta = ops::apply_meta(&state, &[p1, p2, p3], &patch, None).await.unwrap();
    assert!(meta.failed.is_empty(), "{:?}", meta.failed);
    for rel in ["2024/Trip/IMG_0001.xmp", "2024/Trip/IMG_0002.xmp", "2024/Trip/R0000001.xmp"] {
        let x = sidecar(&lib, rel);
        assert_eq!(x.rating, Some(5), "{rel}");
        assert_eq!(x.label.as_deref(), Some("Red"), "{rel}");
        assert_eq!(x.pick, Some(1), "{rel}");
        assert_eq!(x.city.as_deref(), Some("Lisbon"), "{rel}");
        assert!((x.lat.unwrap() - 38.7223).abs() < 1e-4, "{rel}");
        assert!(x.subject.as_ref().unwrap().contains(&"Lisbon".to_string()), "{rel}");
    }
    // Fields Hyperfocal does not manage survive in the other app's sidecar.
    let raw2 = std::fs::read_to_string(lib.join("2024/Trip/IMG_0002.xmp")).unwrap();
    assert!(raw2.contains("Exposure2012") && raw2.contains("Excire Foto"), "{raw2}");
    assert!(sidecar(&lib, "2024/Trip/IMG_0002.xmp").subject.unwrap().contains(&"FromExcire".to_string()));
    assert_eq!(originals(&lib), before, "a metadata write changed an original");

    // 2. Reject the RAW+JPEG pair: the group (and its sidecar) moves together.
    let rej = ops::reject(&state, &[p1]).await.unwrap();
    assert!(rej.failed.is_empty(), "{:?}", rej.failed);
    for n in ["IMG_0001.CR2", "IMG_0001.JPG", "IMG_0001.xmp"] {
        assert!(lib.join("2024/Trip/_Rejected").join(n).is_file(), "{n} not in _Rejected");
        assert!(!lib.join("2024/Trip").join(n).exists(), "{n} still in the event folder");
    }
    // 3. Rejecting the DNG must refuse rather than overwrite the decoy in _Rejected.
    let clash = ops::reject(&state, &[p3]).await.unwrap();
    assert_eq!(clash.failed.len(), 1, "a reject overwrote or replaced an existing file");
    assert!(lib.join("2024/Trip/R0000001.DNG").is_file());
    assert!(lib.join("2024/Trip/R0000001.xmp").is_file(), "the refused move left its sidecar behind");

    // Same bytes, nothing lost or duplicated, just moved.
    let mut moved: Vec<String> = originals(&lib).into_values().collect();
    let mut orig: Vec<String> = before.values().cloned().collect();
    moved.sort();
    orig.sort();
    assert_eq!(moved, orig, "a move changed, lost or duplicated an original");

    // 4. Undo the reject, then the metadata edit.
    let un = ops::undo(&state, &rej.batch).await.unwrap();
    assert!(un.failed.is_empty(), "{:?}", un.failed);
    let un = ops::undo(&state, &meta.batch).await.unwrap();
    assert!(un.failed.is_empty(), "{:?}", un.failed);
    let x2 = sidecar(&lib, "2024/Trip/IMG_0002.xmp");
    assert_eq!(x2.rating, Some(2), "undo did not restore Excire's rating");
    assert_eq!(x2.subject, Some(vec!["FromExcire".to_string()]));
    assert!(std::fs::read_to_string(lib.join("2024/Trip/IMG_0002.xmp")).unwrap().contains("Exposure2012"));
    let x1 = sidecar(&lib, "2024/Trip/IMG_0001.xmp");
    assert_eq!(x1.rating.unwrap_or(0), 0, "undo left the rating on a new sidecar");

    // Every original is back where it was, byte for byte.
    assert_eq!(originals(&lib), before, "originals differ after undo");
    // No temp files, ExifTool backups or other stray files in the library.
    let files = all_files(&lib);
    let expected: Vec<String> = {
        let mut v: Vec<String> = before.keys().cloned().collect();
        v.extend(["2024/Trip/IMG_0001.xmp", "2024/Trip/IMG_0002.xmp", "2024/Trip/R0000001.xmp"].map(String::from));
        v.sort();
        v
    };
    assert_eq!(files, expected, "unexpected files in the library");

    // Backups of the sidecars that existed before each write were kept.
    let backups = all_files(&tmp.path().join("config/backups/sidecars"));
    assert!(backups.iter().any(|b| b.ends_with("IMG_0002.xmp")), "{backups:?}");

    // 5. Read-only mode refuses every write path.
    state.readonly.store(true, std::sync::atomic::Ordering::Relaxed);
    assert!(ops::apply_meta(&state, &[p2], &patch, None).await.is_err());
    assert!(ops::reject(&state, &[p2]).await.is_err());
    assert_eq!(originals(&lib), before);
}
