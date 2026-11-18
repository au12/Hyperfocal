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
        rename_sidecars: true,
        raw_decoder_libraw: false,
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

/// One column of a photo's index row.
fn column<T: rusqlite::types::FromSql>(state: &AppState, id: i64, col: &str) -> Option<T> {
    let c = state.db.get().unwrap();
    c.query_row(&format!("SELECT {col} FROM photos WHERE id = ?1"), [id], |r| r.get(0)).unwrap()
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
    // A RAW+JPEG+HEIF set (cameras that shoot RAW+HEIF), a RAW with an existing Excire
    // sidecar, a DNG, another year, and a decoy already in _Rejected under the DNG's name.
    write_bytes(&lib.join("2024/Trip/IMG_0001.CR2"), 300_000, 1);
    write_bytes(&lib.join("2024/Trip/IMG_0001.JPG"), 40_000, 2);
    write_bytes(&lib.join("2024/Trip/IMG_0001.HIF"), 30_000, 7);
    write_bytes(&lib.join("2024/Trip/IMG_0002.CR3"), 250_000, 3);
    std::fs::write(lib.join("2024/Trip/IMG_0002.xmp"), EXCIRE_SIDECAR).unwrap();
    write_bytes(&lib.join("2024/Trip/R0000001.DNG"), 200_000, 4);
    write_bytes(&lib.join("2024/Trip/_Rejected/R0000001.DNG"), 1_000, 5);
    write_bytes(&lib.join("2023/Other/IMG_9999.CR3"), 100_000, 6);
    let before = originals(&lib);
    assert_eq!(before.len(), 7);

    let state = crate::build_state(test_config(tmp.path())).await.unwrap();
    assert!(!state.is_readonly());
    {
        let st = state.clone();
        tokio::task::spawn_blocking(move || scan_all(&st)).await.unwrap();
    }
    let p1 = photo_id(&state, "2024/Trip", "IMG_0001.CR2");
    let p2 = photo_id(&state, "2024/Trip", "IMG_0002.CR3");
    let p3 = photo_id(&state, "2024/Trip", "R0000001.DNG");
    // The JPEG and HEIF fold into their RAW (the grid badge reads CR2+JPG+HIF).
    assert_eq!(column::<String>(&state, p1, "companion").as_deref(), Some("IMG_0001.JPG,IMG_0001.HIF"));

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

    // 2. Reject the RAW+JPEG+HEIF set: the group (and its sidecar) moves together.
    let rej = ops::reject(&state, &[p1]).await.unwrap();
    assert!(rej.failed.is_empty(), "{:?}", rej.failed);
    for n in ["IMG_0001.CR2", "IMG_0001.JPG", "IMG_0001.HIF", "IMG_0001.xmp"] {
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

/// Two different photos with the same name in one folder (a TIFF saved under its RAW's
/// name, a DNG converted from a CR3 and kept beside it) never share a sidecar, and
/// editing, rejecting or restoring one never touches the other's files.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_name_photos_stay_apart() {
    if !exiftool_available() {
        eprintln!("skipping: exiftool is not installed");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("photos");
    let dir = lib.join("2024/Edit");
    write_bytes(&dir.join("IMG_0003.CR3"), 200_000, 11);
    write_bytes(&dir.join("IMG_0003.JPG"), 30_000, 12);
    write_bytes(&dir.join("IMG_0003.TIF"), 60_000, 13);
    std::fs::write(dir.join("IMG_0003.xmp"), EXCIRE_SIDECAR).unwrap();
    write_bytes(&dir.join("IMG_0004.CR3"), 200_000, 14);
    write_bytes(&dir.join("IMG_0004.DNG"), 150_000, 15);
    std::fs::write(dir.join("IMG_0004.xmp"), EXCIRE_SIDECAR).unwrap();
    let before = originals(&lib);
    let excire = EXCIRE_SIDECAR.as_bytes().to_vec();

    let state = crate::build_state(test_config(tmp.path())).await.unwrap();
    {
        let st = state.clone();
        tokio::task::spawn_blocking(move || scan_all(&st)).await.unwrap();
    }
    let cr3 = photo_id(&state, "2024/Edit", "IMG_0003.CR3");
    let jpg = photo_id(&state, "2024/Edit", "IMG_0003.JPG");
    let tif = photo_id(&state, "2024/Edit", "IMG_0003.TIF");
    let raw4 = photo_id(&state, "2024/Edit", "IMG_0004.CR3");
    let dng = photo_id(&state, "2024/Edit", "IMG_0004.DNG");
    let rating = |id: i64| column::<i32>(&state, id, "rating").unwrap_or(0);
    let files = || all_files(&dir);
    let set = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let rate = |r: i32| -> Patch { serde_json::from_value(serde_json::json!({ "rating": r })).unwrap() };

    // The JPEG folds into its RAW; the TIFF and the DNG are photos of their own, and
    // neither shows the RAW's Excire rating.
    assert_eq!(column::<i64>(&state, jpg, "pair_of"), Some(cr3));
    assert_eq!(column::<i64>(&state, tif, "pair_of"), None);
    assert_eq!(column::<String>(&state, cr3, "companion").as_deref(), Some("IMG_0003.JPG"));
    assert_eq!((rating(cr3), rating(tif), rating(raw4), rating(dng)), (2, 0, 2, 0));

    // 1. Rating the TIFF writes its own sidecar; the RAW's is untouched.
    let r = ops::apply_meta(&state, &[tif], &rate(1), None).await.unwrap();
    assert!(r.failed.is_empty(), "{:?}", r.failed);
    assert_eq!(sidecar(&lib, "2024/Edit/IMG_0003.TIF.xmp").rating, Some(1));
    assert_eq!(std::fs::read(dir.join("IMG_0003.xmp")).unwrap(), excire, "the RAW's sidecar changed");
    assert_eq!((rating(cr3), rating(tif)), (2, 1));

    // 2. Rejecting the TIFF moves the TIFF and its sidecar, nothing else.
    let r = ops::reject(&state, &[tif]).await.unwrap();
    assert!(r.failed.is_empty(), "{:?}", r.failed);
    let mut want = set(&["IMG_0003.CR3", "IMG_0003.JPG", "IMG_0003.xmp", "IMG_0004.CR3", "IMG_0004.DNG", "IMG_0004.xmp"]);
    want.extend(set(&["_Rejected/IMG_0003.TIF", "_Rejected/IMG_0003.TIF.xmp"]));
    want.sort();
    assert_eq!(files(), want);

    // 3. Edited while rejected, it keeps to its own sidecar (no new IMG_0003.xmp there,
    //    which would come back with it and clash with, or turn into, the RAW's).
    let r = ops::apply_meta(&state, &[tif], &rate(3), None).await.unwrap();
    assert!(r.failed.is_empty(), "{:?}", r.failed);
    assert_eq!(sidecar(&lib, "2024/Edit/_Rejected/IMG_0003.TIF.xmp").rating, Some(3));
    assert!(!dir.join("_Rejected/IMG_0003.xmp").exists());

    // 4. Restoring it brings back exactly those two files.
    let r = ops::restore(&state, &[tif]).await.unwrap();
    assert!(r.failed.is_empty(), "{:?}", r.failed);
    assert!(dir.join("IMG_0003.TIF").is_file() && dir.join("IMG_0003.TIF.xmp").is_file());
    assert!(!dir.join("_Rejected/IMG_0003.TIF").exists());

    // 5. Rejecting the RAW takes its JPEG and sidecar and leaves the TIFF alone; an edit
    //    of the TIFF meanwhile still goes to the TIFF's own sidecar.
    let r = ops::reject(&state, &[cr3]).await.unwrap();
    assert!(r.failed.is_empty(), "{:?}", r.failed);
    for n in ["IMG_0003.CR3", "IMG_0003.JPG", "IMG_0003.xmp"] {
        assert!(dir.join("_Rejected").join(n).is_file(), "{n} not in _Rejected");
    }
    assert!(dir.join("IMG_0003.TIF").is_file() && dir.join("IMG_0003.TIF.xmp").is_file());
    let r = ops::apply_meta(&state, &[tif], &rate(4), None).await.unwrap();
    assert!(r.failed.is_empty(), "{:?}", r.failed);
    assert!(!dir.join("IMG_0003.xmp").exists(), "the TIFF took over the rejected RAW's sidecar name");
    assert_eq!(sidecar(&lib, "2024/Edit/IMG_0003.TIF.xmp").rating, Some(4));

    // 6. Restoring the RAW: no clash, and its sidecar is still Excire's, byte for byte.
    let r = ops::restore(&state, &[cr3]).await.unwrap();
    assert!(r.failed.is_empty(), "{:?}", r.failed);
    assert_eq!(std::fs::read(dir.join("IMG_0003.xmp")).unwrap(), excire);
    assert_eq!((rating(cr3), rating(tif)), (2, 4));

    // 7. Rejecting the DNG leaves the CR3's sidecar where it is.
    let r = ops::reject(&state, &[dng]).await.unwrap();
    assert!(r.failed.is_empty(), "{:?}", r.failed);
    assert!(dir.join("_Rejected/IMG_0004.DNG").is_file());
    assert_eq!(std::fs::read(dir.join("IMG_0004.xmp")).unwrap(), excire);
    assert_eq!(rating(raw4), 2);

    // The JPEG half can never be edited or moved on its own.
    assert_eq!(ops::apply_meta(&state, &[jpg], &rate(5), None).await.unwrap().failed.len(), 1);
    assert_eq!(ops::reject(&state, &[jpg]).await.unwrap().failed.len(), 1);

    // An index written by an older version is put right by the next ordinary rescan:
    // first the pairs alone (nothing on disk changed), then a TIFF row that still
    // points at the RAW's sidecar.
    let fid: i64 = column(&state, cr3, "folder_id").unwrap();
    let rescan = || {
        let st = state.clone();
        async move { tokio::task::spawn_blocking(move || scan::scan_folder(&st, fid, false).unwrap()).await.unwrap() }
    };
    {
        let c = state.db.get().unwrap();
        c.execute("UPDATE photos SET pair_of = NULL WHERE id = ?1", [jpg]).unwrap();
        c.execute("UPDATE photos SET companion = 'IMG_0003.JPG,IMG_0003.TIF' WHERE id = ?1", [cr3]).unwrap();
    }
    assert_eq!(rescan().await.changed, 0);
    assert_eq!(column::<i64>(&state, jpg, "pair_of"), Some(cr3));
    assert_eq!(column::<String>(&state, cr3, "companion").as_deref(), Some("IMG_0003.JPG"));
    {
        let c = state.db.get().unwrap();
        c.execute("UPDATE photos SET sidecar = 'IMG_0003.xmp', rating = 2 WHERE id = ?1", [tif]).unwrap();
    }
    assert_eq!(rescan().await.changed, 1);
    assert_eq!(column::<String>(&state, tif, "sidecar").as_deref(), Some("IMG_0003.TIF.xmp"));
    assert_eq!(rating(tif), 4);

    // Originals: same bytes, nothing lost or duplicated.
    let mut now: Vec<String> = originals(&lib).into_values().collect();
    let mut orig: Vec<String> = before.into_values().collect();
    now.sort();
    orig.sort();
    assert_eq!(now, orig);
}

/// darktable/digiKam-style sidecars (`IMG_1.CR3.xmp`) get the standard name when no
/// other photo's metadata can mix with them; the rename is journaled, and undoing it
/// keeps the old name for good.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn darktable_sidecars_get_the_standard_name() {
    if !exiftool_available() {
        eprintln!("skipping: exiftool is not installed");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("photos");
    let dir = lib.join("2023/Hike");
    write_bytes(&dir.join("IMG_0005.CR3"), 200_000, 21);
    std::fs::write(dir.join("IMG_0005.CR3.xmp"), EXCIRE_SIDECAR).unwrap();
    write_bytes(&dir.join("IMG_0006.NEF"), 200_000, 22);
    write_bytes(&dir.join("IMG_0006.JPG"), 30_000, 23);
    std::fs::write(dir.join("IMG_0006.NEF.xmp"), EXCIRE_SIDECAR).unwrap();
    // Not renamed: a same-name TIFF, and a standard sidecar already there.
    write_bytes(&dir.join("IMG_0007.CR3"), 200_000, 24);
    write_bytes(&dir.join("IMG_0007.TIF"), 60_000, 25);
    std::fs::write(dir.join("IMG_0007.CR3.xmp"), EXCIRE_SIDECAR).unwrap();
    write_bytes(&dir.join("IMG_0008.CR3"), 200_000, 26);
    std::fs::write(dir.join("IMG_0008.CR3.xmp"), EXCIRE_SIDECAR).unwrap();
    std::fs::write(dir.join("IMG_0008.xmp"), EXCIRE_SIDECAR.replace("xmp:Rating=\"2\"", "xmp:Rating=\"4\"")).unwrap();
    let before = originals(&lib);

    let state = crate::build_state(test_config(tmp.path())).await.unwrap();
    {
        let st = state.clone();
        tokio::task::spawn_blocking(move || scan_all(&st)).await.unwrap();
    }
    let mut want: Vec<String> = [
        "IMG_0005.CR3", "IMG_0005.xmp", "IMG_0006.JPG", "IMG_0006.NEF", "IMG_0006.xmp", "IMG_0007.CR3", "IMG_0007.CR3.xmp",
        "IMG_0007.TIF", "IMG_0008.CR3", "IMG_0008.CR3.xmp", "IMG_0008.xmp",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    want.sort();
    assert_eq!(all_files(&dir), want);
    // Same bytes under the new name, and the index reads it.
    assert_eq!(std::fs::read(dir.join("IMG_0005.xmp")).unwrap(), EXCIRE_SIDECAR.as_bytes());
    let p5 = photo_id(&state, "2023/Hike", "IMG_0005.CR3");
    assert_eq!(column::<String>(&state, p5, "sidecar").as_deref(), Some("IMG_0005.xmp"));
    assert_eq!(column::<i32>(&state, p5, "rating"), Some(2));
    assert_eq!(column::<i32>(&state, photo_id(&state, "2023/Hike", "IMG_0008.CR3"), "rating"), Some(4));

    // Undo the renames: the old names come back and later scans leave them alone.
    let batch: String = state
        .db
        .get()
        .unwrap()
        .query_row("SELECT batch FROM journal WHERE kind = 'rename' LIMIT 1", [], |r| r.get(0))
        .unwrap();
    let r = ops::undo(&state, &batch).await.unwrap();
    assert!(r.failed.is_empty(), "{:?}", r.failed);
    assert_eq!(r.ok.len(), 2);
    {
        let st = state.clone();
        tokio::task::spawn_blocking(move || scan_all(&st)).await.unwrap();
    }
    assert!(dir.join("IMG_0005.CR3.xmp").is_file() && dir.join("IMG_0006.NEF.xmp").is_file());
    assert!(!dir.join("IMG_0005.xmp").exists() && !dir.join("IMG_0006.xmp").exists());
    assert_eq!(column::<i32>(&state, p5, "rating"), Some(2));
    // An edit then goes into the sidecar the photo shows, under the name you kept.
    let patch: Patch = serde_json::from_value(serde_json::json!({ "rating": 5 })).unwrap();
    let r = ops::apply_meta(&state, &[p5], &patch, None).await.unwrap();
    assert!(r.failed.is_empty(), "{:?}", r.failed);
    assert_eq!(sidecar(&lib, "2023/Hike/IMG_0005.CR3.xmp").rating, Some(5));
    assert!(!dir.join("IMG_0005.xmp").exists());

    let mut now: Vec<String> = originals(&lib).into_values().collect();
    let mut orig: Vec<String> = before.into_values().collect();
    now.sort();
    orig.sort();
    assert_eq!(now, orig);
}
